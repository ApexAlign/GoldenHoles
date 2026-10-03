//! One renderer-agnostic per-tick client controller. A render shell (Bevy, or a test) feeds it input events,
//! datagrams and a clock, and draws what `view()` returns; there is no gameplay logic outside this file.
//!
//! ```text
//! shell:   button_event / look / touch_event        (any rate)
//!          on_datagram(bytes, local_tick)           (whenever the socket yields one)
//!          on_own_state(OwnAck)                     (owner-only movement state, see own_state.rs)
//!          sim_tick(&InputFrame, local_tick) -> Option<datagram to send>   (exactly once per 60 Hz tick)
//!          view(local_tick) -> RenderView           (once per rendered frame)
//! ```
//!
//! Timeline: every `sim_tick` stamps an intent with the server tick it should execute on (`NetClient::pace_tick`),
//! predicts it locally through the same `step_player` the server runs, and records the result. When the server's
//! own-state for an acked tick arrives, `Reconciler` compares it with what was predicted; on a mismatch the
//! character is restored to the server's state and the buffered intents are replayed. The drawn position is
//! `simulated + visual_offset`, which decays to zero, so a correction never pops on screen.

use std::collections::{HashMap, VecDeque};

use shinobi_proto::bits::DecodeResult;
use shinobi_proto::datagram::KickReason;
use shinobi_proto::gameplay::{Board, HitMsg};
use shinobi_proto::intent::{Buttons, PlayerIntent};
use shinobi_proto::quant::{dq_pitch, dq_yaw};
use shinobi_proto::quant::dq_vel;
use shinobi_proto::snapshot::{EntityId, PROJECTILE_ID_BASE};
use shinobi_proto::TICK_DT;
use shinobi_sim::balance::FeedbackBalance;
use shinobi_sim::combat::aim_assist::{LockEdge, LockPhase};
use shinobi_sim::blocks::{BlockCfg, BlockEvent, BlockInfo, Blocks, Piece, PieceKind, SCALE_ONE};
use shinobi_sim::character::{Cfg, CharacterSystem, GrappleEvent, MoveState, PhysicsWorld, PlayerInput};
use shinobi_sim::combat::katana::{predict_slice, KatanaCfg};
use shinobi_sim::combat::shuriken::ShurikenCfg;
use shinobi_sim::combat::{Aim as CombatAim, CombatEvent, CombatInput, CombatSystem, GROUP_HITBOX, GROUP_MELEE, GROUP_PLAYER, GROUP_PROJECTILE};
use shinobi_sim::events::NetEvent;
use shinobi_sim::level::LevelDesc;
use shinobi_sim::movement::{refresh_solid, step_player_with};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::{to_player_input, view_dir};

use crate::camera::{CameraFeel, CameraInput, CameraPose, FpsCamera, FpsCameraCfg, WeaponPose};
use crate::settings::Settings;

/// Vertical sprint-bob amplitude (m) when the head-bob setting is on. Off by default.
const HEAD_BOB_AMP: f32 = 0.018;
use crate::input::{build_intent, Aim, ButtonLatch, TouchController, TouchEvent};
use crate::interp::Sample;
use crate::netclient::NetClient;
use crate::reconcile::Reconciler;

/// Physical actions a shell maps its keys / touch buttons / gamepad onto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Jump,
    Dash,
    Roll,
    Slice,
    Fire,
    Cling,
    Slide,
    /// Wrist grapple: fire at the crosshair, press again to release (one-tick edge on the `zip` wire bit).
    Grapple,
    /// Print a floor where the ghost preview points (`N`; one-tick edge on the `build` wire bit).
    BuildFloor,
    /// Print a wall where the ghost preview points (`M`; `build` edge + `build_wall`).
    BuildWall,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ButtonEvent {
    pub action: Action,
    pub down: bool,
}

/// Everything the shell observed since the previous `sim_tick`.
#[derive(Clone, Default, Debug)]
pub struct InputFrame {
    /// View-frame stick: x strafe right, z forward, each -1..1 (`input::keys_to_stick` gives this for WASD).
    pub stick: (f32, f32),
    /// Accumulated look motion in device units (Pointer Lock `movementX/Y`); right/down positive.
    pub look: (f32, f32),
    /// Presses and releases in the order they happened. A tap shorter than a tick still registers.
    pub buttons: Vec<ButtonEvent>,
}

/// Owner-only authoritative movement state for one simulated tick. `tick` is the intent tick the server had just
/// executed. `pos` is the snapshot's (quantised) entity position and is IGNORED when `own.pos` is present, which
/// it always is: the reconciler uses the exact one so prediction can match bit-for-bit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OwnAck {
    pub tick: u32,
    pub pos: [f32; 3],
    pub own: OwnState,
}

#[derive(Clone, Copy)]
pub struct ClientGameCfg {
    pub sim: Cfg,
    pub camera: FpsCameraCfg,
    /// Ticks of headroom the intent clock gets over the server-clock estimate. It must cover RTT plus jitter
    /// (the estimate lags the real server tick by one one-way trip), and stay under `MAX_FUTURE_TICKS` (12).
    pub lead_ticks: u32,
    /// Look sensitivity, radians per device unit.
    pub rad_per_unit: f32,
    /// Hit-stop after a katana hit / kill, seconds (Balance `feedback.hitstop_*`). Client-only: the sim never pauses.
    pub hitstop_slash: f32,
    pub hitstop_kill: f32,
}

impl Default for ClientGameCfg {
    fn default() -> Self {
        Self {
            sim: Cfg::default(),
            camera: FpsCameraCfg::default(),
            lead_ticks: 8,
            rad_per_unit: 0.0022,
            hitstop_slash: FeedbackBalance::default().hitstop_slash_ms / 1000.0,
            hitstop_kill: FeedbackBalance::default().hitstop_kill_ms / 1000.0,
        }
    }
}

/// Local predicted character.
struct Predicted {
    world: PhysicsWorld,
    ch: CharacterSystem,
    /// Local copy of the combat state machine: cosmetics (slash / throw / recoil) and the katana lunge. Its
    /// `PlayerInput` mutation drives movement; the katana cooldown is mirrored into `ch` (and so into `OwnState`).
    /// Replays do not re-run it: `lunge_input` makes the same movement decision from the restored cooldown.
    combat: CombatSystem,
    katana: KatanaCfg,
    /// Movement broad phase, kept across ticks (rebuilt each tick, but without reallocating).
    solid: QueryPipeline,
    now: f32,
    pos: [f32; 3],
    prev_pos: [f32; 3],
    /// Crosshair is on a hookable surface (probed every tick against the predicted world).
    hookable: bool,
    /// Client clock (ticks) at the most recent `sim_tick`; interpolation alpha is measured from here.
    sim_local: f64,
    /// Replicated + predicted blocks as colliders in `world` (same code as the server: `shinobi_sim::blocks`).
    blocks: Blocks,
    /// Blocks placed locally and not yet confirmed by the server: `(cell, local tick placed)`.
    block_pending: Vec<([i32; 3], u32)>,
    /// Sensor hitboxes standing in for the other players (placed at where our screen shows them), so the
    /// predicted combat finds the same lock-on target the server will.
    proxies: HashMap<EntityId, ColliderHandle>,
}

/// Unconfirmed predicted blocks older than this many ticks are dropped (the server refused them).
const BLOCK_PENDING_TICKS: u32 = 45;

impl Predicted {
    /// Make `blocks` (the server's live set) the colliders of the predicted world; our own recent unconfirmed
    /// placements stay until the server confirms them or `BLOCK_PENDING_TICKS` pass.
    fn sync_blocks(&mut self, want: &[BlockInfo], tick: u32) {
        let mut changed = false;
        for b in want {
            let same = self.blocks.pieces().into_iter().any(|p| p.cell == b.cell && p.piece == b.piece && p.scale_q == b.scale_q);
            if same {
                continue;
            }
            // a predicted piece that the server placed differently (kind / scale) is replaced by the server's
            changed |= self.blocks.remove(&mut self.world, b.cell);
            changed |= self.blocks.place(&mut self.world, b.owner, b.cell, b.piece, b.scale_q, tick, &[]).is_ok();
        }
        self.block_pending.retain(|(cell, t)| tick.wrapping_sub(*t) < BLOCK_PENDING_TICKS && !want.iter().any(|b| b.cell == *cell));
        let stale: Vec<[i32; 3]> = self
            .blocks
            .iter()
            .into_iter()
            .map(|(_, c)| c)
            .filter(|c| !want.iter().any(|b| b.cell == *c) && !self.block_pending.iter().any(|(p, _)| p == c))
            .collect();
        for cell in stale {
            changed |= self.blocks.remove(&mut self.world, cell);
        }
        if changed {
            self.world.queries.update(&self.world.colliders);
        }
    }

    /// Move/create/remove the remote players' proxy hitboxes. Returns the proxies of teammates (never targeted).
    fn sync_proxies(&mut self, cfg: &Cfg, remotes: &[(EntityId, [f32; 3], bool)]) -> Vec<ColliderHandle> {
        let gone: Vec<EntityId> = self.proxies.keys().copied().filter(|id| !remotes.iter().any(|r| r.0 == *id)).collect();
        for id in gone {
            if let Some(h) = self.proxies.remove(&id) {
                self.world.colliders.remove(h, &mut self.world.islands, &mut self.world.bodies, false);
            }
        }
        let mut allies = Vec::new();
        for (id, pos, ally) in remotes {
            let h = *self.proxies.entry(*id).or_insert_with(|| {
                self.world.colliders.insert(
                    ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                        .sensor(true)
                        .collision_groups(InteractionGroups::new(GROUP_HITBOX, GROUP_PROJECTILE | GROUP_MELEE))
                        .user_data(*id as u128)
                        .build(),
                )
            });
            if let Some(col) = self.world.colliders.get_mut(h) {
                col.set_position(Isometry::translation(pos[0], pos[1], pos[2]));
            }
            if *ally {
                allies.push(h);
            }
        }
        allies
    }

    /// The build press, run through the same `Blocks::try_build` the server uses.
    fn predict_build(&mut self, owner: u16, tick: u32, kind: PieceKind, aim: &CombatAim) {
        self.world.queries.update(&self.world.colliders);
        let f = aim.forward();
        let mut ev = Vec::new();
        let body = self.ch.body;
        // kills only scale pieces (a hook, 0 by default); the server's value arrives with the confirming BlockPlaced
        self.blocks.try_build(&mut self.world, tick, owner, kind, 0, [aim.origin.x, aim.origin.y, aim.origin.z], [f.x, f.y, f.z], body, &[self.pos], &mut ev);
        for e in ev {
            if let BlockEvent::Placed { cell, .. } = e {
                self.block_pending.push((cell, tick));
                self.world.queries.update(&self.world.colliders);
            }
        }
    }

    /// Where a press would place a block right now, and whether it would be accepted.
    fn build_ghost(&mut self, owner: u16, tick: u32, kind: PieceKind, aim: &CombatAim) -> BuildGhost {
        self.world.queries.update(&self.world.colliders);
        let f = aim.forward();
        let cell = self.blocks.target_cell(&self.world, [aim.origin.x, aim.origin.y, aim.origin.z], [f.x, f.y, f.z], self.ch.body);
        let valid = self.blocks.is_free(cell) && !self.blocks.overlaps_player(cell, &[self.pos]);
        let piece = Piece::for_press(kind, [f.x, f.y, f.z]);
        let (center, half) = self.blocks.geometry(cell, piece, SCALE_ONE);
        BuildGhost { cell, piece, center, half, valid, ready: self.blocks.ready(owner, tick) }
    }

    fn step(&mut self, input: &PlayerInput) -> [f32; 3] {
        refresh_solid(&mut self.solid, &self.world.colliders);
        step_player_with(&mut self.ch, input, &mut self.world, self.now, TICK_DT, &mut self.solid);
        self.now += TICK_DT;
        let t = self.world.bodies[self.ch.body].translation();
        self.prev_pos = self.pos;
        self.pos = [t.x, t.y, t.z];
        self.pos
    }

    /// Can the grapple grab something along this view direction from here? (Crosshair tint.)
    fn probe_hook(&mut self, aim_yaw: u16, aim_pitch: u16) {
        refresh_solid(&mut self.solid, &self.world.colliders);
        std::mem::swap(&mut self.world.queries, &mut self.solid);
        let eye = vector![self.pos[0], self.pos[1] + self.ch.eye_above(), self.pos[2]];
        self.hookable = self.ch.grapple_probe(&self.world, eye, view_dir(aim_yaw, aim_pitch)).is_some();
        std::mem::swap(&mut self.world.queries, &mut self.solid);
    }
}

/// The tick's `PlayerInput` as the server will build it: stick + buttons, plus the katana lunge decision made
/// from the (restored or live) cooldown mirror in `ch`. Shared by the catch-up and the replay hook.
fn lunge_input(katana: &KatanaCfg, it: &PlayerIntent, ch: &mut CharacterSystem) -> PlayerInput {
    let mut input = to_player_input(it);
    let mut cd = ch.slice_cooldown();
    let aim = CombatAim { origin: vector![0.0, 0.0, 0.0], yaw: dq_yaw(it.aim_yaw), pitch: 0.0 };
    predict_slice(katana.cooldown, &mut cd, TICK_DT, it.buttons.has(Buttons::SLICE), ch.dash_ready(), aim.flat_forward(), &mut input);
    ch.set_slice_cooldown(cd);
    input
}

/// Movement flags for edge detection.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct MoveFlags {
    grounded: bool,
    wall_running: bool,
    dashing: bool,
    sliding: bool,
}

/// A predicted katana swing (cosmetic; the authoritative result arrives with the hit messages).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PredictedSlash {
    /// The dash lunge fired (false: on cooldown, short swing only).
    pub lunge: bool,
    pub dir: [f32; 3],
    pub flip: bool,
}

/// A predicted katana hit.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct PredictedSlashHit {
    pub point: [f32; 3],
    pub killed: bool,
}

/// Local-player effects state for the shell's particles, audio and viewmodel.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct LocalFx {
    // -- continuous, as of the newest predicted tick --
    pub grounded: bool,
    pub wall_running: bool,
    pub dashing: bool,
    pub rolling: bool,
    pub sliding: bool,
    /// Horizontal speed, m/s.
    pub speed: f32,
    pub dash_ready: bool,
    /// Katana / shuriken cooldown remaining, 1.0 = just used, 0.0 = ready.
    pub slice_cooldown: f32,
    pub throw_cooldown: f32,
    // -- edges since the previous `view()`; a tick that falls between two views is never lost --
    pub jumped: bool,
    pub double_jumped: bool,
    pub landed: bool,
    pub wall_run_started: bool,
    pub dash_started: bool,
    pub dash_ended: bool,
    pub slide_started: bool,
    /// Fall speed (m/s) of the hardest landing in the interval; 0 = none. Drives landing audio / dust.
    pub landing_speed: f32,
    /// The server moved the local player by more than the smoothing limit (respawn, match-start reset): the
    /// shell should cut, not slide, camera effects.
    pub teleported: bool,
    pub slash: Option<PredictedSlash>,
    /// The predicted katana swing connected: where, and whether it killed (client-side prediction: the server's
    /// kill arrives as a kill event; this is never `killed` on its own).
    pub slash_hit: Option<PredictedSlashHit>,
    /// The predicted swing found nothing: play the whiff.
    pub slash_miss: bool,
    /// Freeze the viewmodel / shake the camera this long (seconds) after a confirmed katana hit. Client-only: the
    /// sim never pauses. Longest request in the interval.
    pub hit_stop: f32,
    /// Shuriken thrown this interval: its direction.
    pub threw: Option<[f32; 3]>,
    // -- wrist grapple --
    /// Rope attached as of the newest predicted tick.
    pub grappling: bool,
    /// Ready to fire (off cooldown and not already on the rope).
    pub grapple_ready: bool,
    /// 1.0 = just released / missed, 0.0 = ready (same convention as the other cooldowns).
    pub grapple_cooldown: f32,
    /// The crosshair is on something the hook can grab right now (client-side ray in the predicted world).
    pub grapple_hookable: bool,
    /// Hooked this interval: the anchor point.
    pub grapple_attached: Option<[f32; 3]>,
    pub grapple_released: bool,
    /// Fired at nothing hookable this interval ('no target' fizzle).
    pub grapple_missed: bool,
}

/// The lock-on state for the shell: three dots on `pos` that converge as `progress` goes 0 -> 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LockOn {
    /// Player id of the target (`RenderView.remotes` key).
    pub target: EntityId,
    /// Aim point (chest) in world space.
    pub pos: [f32; 3],
    /// 0..=1. Rises over `combat.lock_secs` while the target stays in the cone; fades to 0 during `Break`.
    pub progress: f32,
    /// Progress reached 1 and the target is still held.
    pub locked: bool,
    /// `Acquire` (dots converge), `Locked` (snap + pulse), `Break` (scatter / fade).
    pub phase: LockPhase,
}

/// Block placement preview for the shell: draw a translucent cube at `center` (cyan when `valid`, red otherwise);
/// `ready` is false during the placement cooldown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildGhost {
    pub cell: [i32; 3],
    pub piece: Piece,
    /// Slab centre and half extents (draw a box of 2 * half at center).
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub valid: bool,
    pub ready: bool,
}

/// What the shell draws. `Default` so shells and tests can use `..Default::default()` and new fields never break them.
#[derive(Clone, Debug, Default)]
pub struct RenderView {
    pub fx: LocalFx,
    /// Feedback strengths from the settings. The camera already applies `roll`, `thrust` and `fov_kick`; the shell
    /// multiplies `FxState` output by `shake` (look kick, vignette punch) and `warp` (lens warp, dash FOV add).
    pub feel: CameraFeel,
    /// No WELCOME within the connect timeout (server down / blocked / version mismatch).
    pub connect_failed: bool,
    /// Exact own health in server units (None before the first extras block).
    pub hp: Option<f32>,
    /// Hits since the previous view: dealt (hit marker) and taken (damage direction).
    pub hits: Vec<HitMsg>,
    pub board: Option<Board>,
    pub connected: bool,
    pub kicked: Option<KickReason>,
    pub camera: CameraPose,
    pub weapon: WeaponPose,
    /// Camera position: local position plus eye height and the camera's vertical thrust.
    pub eye: [f32; 3],
    /// Local player centre as drawn (interpolated between sim ticks, plus the decaying correction offset).
    /// Falls back to the server snapshot until the predictor has its first authoritative state.
    pub local_pos: Option<[f32; 3]>,
    /// Remaining reconcile smoothing; zero when prediction and server agree.
    pub correction_offset: [f32; 3],
    /// Other players, interpolated at the render tick.
    pub remotes: Vec<(EntityId, Sample)>,
    /// Reliable events (walls, obstacles) received since the previous view.
    pub events: Vec<NetEvent>,
    pub aim: Aim,
    /// Shurikens in flight: world position, direction (own from prediction, others from the snapshot).
    pub discs: Vec<([f32; 3], [f32; 3])>,
    /// Owner player id of each entry in `discs` (same index), for per-player colours. 0 = unknown.
    pub disc_owners: Vec<u16>,
    /// Own wrist-grapple rope as (hand/eye, anchor) while attached, from the prediction.
    pub rope: Option<([f32; 3], [f32; 3])>,
    /// Placement preview for the block under the crosshair (None before prediction starts or while dead).
    pub build_ghost: Option<BuildGhost>,
    /// Predator lock-on (docs/lock-on-and-hud.md): `None` while searching, else the target as the predictor sees it.
    pub lock: Option<LockOn>,
    /// Lock-on edges since the previous view, in order, for audio / flashes to trigger on.
    pub lock_edges: Vec<LockEdge>,
    /// Every live block as `(owner, cell)`; centre = `(cell + 0.5) * block_size`. Server-confirmed plus our own pending.
    pub blocks: Vec<BlockInfo>,
    /// Edge length of a block cell, metres. Slab geometry: `block_geometry`.
    pub block_size: f32,
    /// Slab thickness, metres.
    pub block_thickness: f32,
}

impl RenderView {
    /// (yaw, pitch) the camera must render with: exactly the aim that goes out in intents, so the crosshair is
    /// where the shot goes. Recoil, bob, roll and the FX look-kick are NOT added here (roll / FOV are separate
    /// pose channels; the kick was dropped from the view because it moved the crosshair off the real aim).
    pub fn look_angles(&self) -> (f32, f32) {
        (self.aim.yaw, self.aim.pitch)
    }
}

pub struct ClientGame {
    pub cfg: ClientGameCfg,
    pub net: NetClient,
    pub aim: Aim,
    pub settings: Settings,
    screen: (f32, f32),
    pub reconciler: Reconciler,
    pub camera: FpsCamera,
    level: LevelDesc,
    latch: ButtonLatch,
    zip_edge: bool,
    /// Pending build press for the next tick: `Some(kind)` (latched until the tick, so a sub-tick tap is not lost).
    build_edge: Option<PieceKind>,
    /// Piece the ghost previews: the one last printed (floor until the first press).
    build_sel: PieceKind,
    /// Tick of the newest intent sent (cooldown readiness for the ghost).
    last_input_tick: u32,
    /// Lock-on edges (acquire / lock / break) since the previous `view()`.
    lock_edges: Vec<LockEdge>,
    touch: TouchController,
    pred: Option<Predicted>,
    /// Intents sent before the predictor has an authoritative starting state; replayed once it has.
    pending: VecDeque<PlayerIntent>,
    pose: (CameraPose, WeaponPose),
    /// Edge FX accumulated across sim ticks since the last `view()` (replays never add to it).
    fx_edges: LocalFx,
    throw_cd_left: f32,
    prev_flags: MoveFlags,
    /// Number of corrections applied (replays), for stats / debug overlay.
    pub corrections: u32,
}

impl ClientGame {
    pub fn new(level: LevelDesc, mut cfg: ClientGameCfg) -> Self {
        cfg.camera.slide_drop = cfg.sim.slide_eye_drop; // one source for the eye drop: camera and shot origin agree
        Self {
            camera: FpsCamera::new(cfg.camera),
            cfg,
            net: NetClient::default(),
            aim: Aim::default(),
            settings: Settings::default(),
            screen: (1280.0, 720.0),
            reconciler: Reconciler::default(),
            level,
            latch: ButtonLatch::default(),
            zip_edge: false,
            build_edge: None,
            build_sel: PieceKind::Floor,
            last_input_tick: 0,
            lock_edges: Vec::new(),
            touch: TouchController::new(1280.0, 90.0),
            pred: None,
            pending: VecDeque::new(),
            pose: (CameraPose::default(), WeaponPose::default()),
            fx_edges: LocalFx::default(),
            throw_cd_left: 0.0,
            prev_flags: MoveFlags::default(),
            corrections: 0,
        }
    }

    // ---- input (any rate) ----

    pub fn button_event(&mut self, e: ButtonEvent) {
        match e.action {
            Action::Jump => self.latch.set_jump(e.down),
            Action::Dash => self.latch.set(Buttons::DASH, e.down),
            Action::Roll => self.latch.set(Buttons::ROLL, e.down),
            Action::Slice => self.latch.set(Buttons::SLICE, e.down),
            Action::Fire => self.latch.set(Buttons::FIRE, e.down),
            Action::Cling => self.latch.set(Buttons::WALL_CLING, e.down),
            Action::Slide => self.latch.set(Buttons::SLIDE, e.down),
            Action::Grapple => self.zip_edge |= e.down, // latched until the next sim tick, so a sub-tick tap is not lost
            Action::BuildFloor if e.down => {
                self.build_edge = Some(PieceKind::Floor);
                self.build_sel = PieceKind::Floor;
            }
            Action::BuildWall if e.down => {
                self.build_edge = Some(PieceKind::Wall);
                self.build_sel = PieceKind::Wall;
            }
            Action::BuildFloor | Action::BuildWall => {}
        }
    }

    /// Mouse look in device units (Pointer Lock `movementX/Y`); sensitivity and invert-Y come from the settings.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.settings.look(&mut self.aim, dx, dy, false);
    }

    pub fn touch_event(&mut self, e: TouchEvent) {
        self.touch.event(e);
    }

    pub fn set_screen_width(&mut self, w: f32) {
        self.touch.screen_w = w;
        self.screen.0 = w;
    }

    /// Window size: sizes the touch stick (a fraction of the screen height) and halves.
    pub fn set_screen(&mut self, w: f32, h: f32) {
        self.screen = (w, h);
        self.settings.apply_to_touch(&mut self.touch, w, h);
    }

    /// Apply player settings live: look sensitivity (mouse and touch pad), invert-Y, base FOV, touch stick,
    /// and the feedback strengths (`Settings::feel`: FX intensity / reduced motion). Safe to call every frame.
    pub fn apply_settings(&mut self, s: &Settings) {
        let s = s.clamped();
        self.settings = s;
        self.cfg.rad_per_unit = s.mouse_sens;
        self.camera.cfg.base_fov = s.fov_deg;
        self.cfg.camera.base_fov = s.fov_deg;
        self.camera.feel = s.feel();
        self.camera.cfg.bob_amp = if s.head_bob { HEAD_BOB_AMP } else { 0.0 };
        s.apply_to_touch(&mut self.touch, self.screen.0, self.screen.1);
    }

    // ---- network in ----

    pub fn on_datagram(&mut self, buf: &[u8], local_tick: f64) -> DecodeResult<()> {
        self.net.on_datagram(buf, local_tick)
    }

    /// Authoritative own-state for an acked intent tick. Creates the predictor on the first one.
    pub fn on_own_state(&mut self, ack: OwnAck) {
        let Some(pred) = &mut self.pred else {
            self.start_prediction(ack);
            return;
        };
        let sim = pred.pos;
        let off = self.reconciler.visual_offset;
        let rendered = [sim[0] + off[0], sim[1] + off[1], sim[2] + off[2]];
        let katana = pred.katana;
        let r = self.reconciler.on_server_own_state(
            ack.tick,
            ack.pos,
            &ack.own,
            rendered,
            &mut pred.ch,
            &mut pred.world,
            &mut |it, ch| lunge_input(&katana, it, ch),
        );
        if let Some(p) = r {
            self.corrections += 1;
            pred.pos = p;
            pred.prev_pos = p; // the offset already keeps the screen where it was
            pred.combat.set_slice_cooldown_left(pred.ch.slice_cooldown());
            // a jump the reconciler refused to smooth (offset reset) is a server teleport: respawn, match start
            let jump = (0..3).map(|i| (rendered[i] - p[i]).powi(2)).sum::<f32>().sqrt();
            if jump > self.reconciler.snap_dist {
                self.fx_edges.teleported = true;
            }
        }
    }

    fn start_prediction(&mut self, ack: OwnAck) {
        let cfg = self.cfg.sim;
        let mut world = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        self.level.insert_into(&mut world);
        let body = world.bodies.insert(
            RigidBodyBuilder::kinematic_position_based().translation(vector![ack.own.pos[0], ack.own.pos[1], ack.own.pos[2]]).build(),
        );
        world.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                .collision_groups(InteractionGroups::new(GROUP_PLAYER, Group::ALL))
                .build(),
            body,
            &mut world.bodies,
        );
        let mut ch = CharacterSystem::new(body, cfg);
        ch.restore_own(&mut world, &ack.own);
        let combat = CombatSystem::ninja(&cfg);
        let katana = KatanaCfg::for_character(&cfg);
        let mut p = Predicted {
            world,
            ch,
            combat,
            katana,
            solid: QueryPipeline::new(),
            now: (ack.tick + 1) as f32 * TICK_DT,
            pos: ack.own.pos,
            prev_pos: ack.own.pos,
            hookable: false,
            sim_local: 0.0,
            // the server evicts at its cap and says so (BlockRemoved): never evict locally on our own
            blocks: Blocks::new(BlockCfg { max_per_owner: usize::MAX, ..BlockCfg::default() }),
            block_pending: Vec::new(),
            proxies: HashMap::new(),
        };
        // catch up: every intent already sent after the acked tick
        let replay: Vec<_> = self.pending.drain(..).filter(|i| i.tick > ack.tick).collect();
        for it in replay {
            let input = lunge_input(&p.katana, &it, &mut p.ch);
            let pos = p.step(&input);
            self.reconciler.record(it, pos);
        }
        p.combat.set_slice_cooldown_left(p.ch.slice_cooldown());
        p.prev_pos = p.pos;
        self.pred = Some(p);
    }

    // ---- the 60 Hz tick ----

    /// Exactly once per fixed sim tick. Returns the datagram to send (HELLO while connecting, then one intent
    /// packet per tick), or `None` (kicked, or waiting for the server clock).
    pub fn sim_tick(&mut self, frame: &InputFrame, local_tick: f64) -> Option<Vec<u8>> {
        for e in &frame.buttons {
            self.button_event(*e);
        }
        let (dx, dy) = frame.look;
        if dx != 0.0 || dy != 0.0 {
            self.settings.look(&mut self.aim, dx, dy, false);
        }
        let (tx, ty) = self.touch.take_look();
        if tx != 0.0 || ty != 0.0 {
            self.settings.look(&mut self.aim, tx, ty, true); // the aim pad has its own sensitivity
        }
        let stick = if frame.stick != (0.0, 0.0) { frame.stick } else { self.touch.stick() };
        self.reconciler.decay(TICK_DT);

        if self.net.kicked().is_some() {
            return None;
        }
        if self.net.player_id().is_none() {
            // retransmit policy + cookie handling live in NetClient (v3 handshake)
            return self.net.hello_due(local_tick).map(|h| h.to_vec());
        }
        let tick = self.net.pace_tick(local_tick, self.cfg.lead_ticks)?;
        if self.net.stamp_held() {
            return None; // the intent lead just shrank: this local tick maps onto the previous stamp, nothing to predict
        }
        let mut intent = build_intent(tick, stick, &self.aim, self.latch.sample());
        intent.zip = std::mem::take(&mut self.zip_edge);
        let press = self.build_edge.take();
        intent.build = press.is_some();
        intent.build_wall = press == Some(PieceKind::Wall);
        self.last_input_tick = tick;
        let own_id_for_blocks = self.net.player_id().unwrap_or(0);

        match &mut self.pred {
            Some(p) => {
                // blocks before anything moves: the world this tick collides with = the server's blocks + our own unconfirmed ones
                let my_id = own_id_for_blocks;
                p.sync_blocks(&self.net.block_pieces(), tick);
                if let Some(kind) = press {
                    let aim = CombatAim { origin: vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]], yaw: dq_yaw(intent.aim_yaw), pitch: dq_pitch(intent.aim_pitch) };
                    p.predict_build(my_id, tick, kind, &aim);
                }
                // combat first, like the server: cosmetics + cooldown mirror (lunge is not applied, see Predicted)
                let mut ev = Vec::new();
                let mut sink = to_player_input(&intent);
                // the other players as our screen shows them: the lock-on (and the assisted throw) read these, so the
                // predictor finds the same target as the server's rewound hitboxes
                let own_id = self.net.player_id();
                let my_team = self.net.my_team();
                let remotes: Vec<(EntityId, [f32; 3], bool)> = match (self.net.render_tick(local_tick), self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t))) {
                    (Some(rt), Some(newest)) => newest
                        .entities
                        .iter()
                        .filter(|(id, _)| Some(*id) != own_id && *id < PROJECTILE_ID_BASE)
                        .filter_map(|(id, _)| {
                            let ally = my_team.is_some() && self.net.team_of(*id) == my_team;
                            self.net.interp.sample(*id, rt).map(|s| (*id, s.pos, ally))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                p.combat.allies = p.sync_proxies(&self.cfg.sim, &remotes);
                p.world.queries.update(&p.world.colliders); // combat now runs every tick (lock-on tracks the cone continuously)
                let aim = CombatAim {
                    origin: vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]],
                    yaw: dq_yaw(intent.aim_yaw),
                    pitch: dq_pitch(intent.aim_pitch),
                };
                p.combat.update(
                    TICK_DT,
                    p.now,
                    &CombatInput {
                        fire: intent.buttons.has(Buttons::FIRE),
                        slice: intent.buttons.has(Buttons::SLICE),
                        ultimate: intent.ultimate,
                    },
                    &aim,
                    &p.world,
                    &p.ch,
                    &mut sink,
                    &mut ev,
                );
                p.ch.set_slice_cooldown(p.combat.slice_cooldown_left()); // same mirror the server publishes
                if let Some(edge) = p.combat.lock_edge {
                    self.lock_edges.push(edge);
                }
                let vy_before = p.ch.vel.y; // fall speed for the landing dip (the controller zeroes it on landing)
                let pos = p.step(&sink);
                p.sim_local = local_tick;
                self.reconciler.record(intent, pos);
                p.probe_hook(intent.aim_yaw, intent.aim_pitch);
                match p.ch.grapple_event {
                    GrappleEvent::Attached { anchor, .. } => self.fx_edges.grapple_attached = Some([anchor.x, anchor.y, anchor.z]),
                    GrappleEvent::Released => self.fx_edges.grapple_released = true,
                    GrappleEvent::Miss { .. } => self.fx_edges.grapple_missed = true,
                    GrappleEvent::None => {}
                }

                // cooldown mirror + cosmetic edges
                self.throw_cd_left = (self.throw_cd_left - TICK_DT).max(0.0);
                for e in ev {
                    match e {
                        CombatEvent::Slash { flip, dir, lunge, .. } => {
                            self.fx_edges.slash = Some(PredictedSlash { lunge, dir: [dir.x, dir.y, dir.z], flip });
                        }
                        CombatEvent::ShurikenThrown { dir, .. } => {
                            self.throw_cd_left = ShurikenCfg::default().cooldown;
                            self.fx_edges.threw = Some([dir.x, dir.y, dir.z]);
                        }
                        CombatEvent::SlashHit { point, killed, .. } => {
                            self.fx_edges.slash_hit = Some(PredictedSlashHit { point: [point.x, point.y, point.z], killed });
                            let stop = if killed { self.cfg.hitstop_kill } else { self.cfg.hitstop_slash };
                            self.fx_edges.hit_stop = self.fx_edges.hit_stop.max(stop);
                        }
                        CombatEvent::SlashMiss { .. } => self.fx_edges.slash_miss = true,
                        CombatEvent::Recoil { pitch_deg, .. } => self.camera.fire_kick(pitch_deg.to_radians(), 0.02),
                        _ => {}
                    }
                }
                let flags = MoveFlags {
                    grounded: p.ch.state == MoveState::Grounded,
                    wall_running: matches!(p.ch.state, MoveState::WallRun { .. }),
                    dashing: p.ch.evading().is_some(),
                    sliding: p.ch.sliding(),
                };
                let prev = self.prev_flags;
                let edges = &mut self.fx_edges;
                edges.jumped |= prev.grounded && !flags.grounded && p.ch.vel.y > 0.0;
                edges.landed |= !prev.grounded && flags.grounded;
                edges.double_jumped |= p.ch.double_jumped;
                edges.wall_run_started |= !prev.wall_running && flags.wall_running;
                edges.dash_started |= !prev.dashing && flags.dashing;
                edges.slide_started |= !prev.sliding && flags.sliding;
                let landed_speed = if !prev.grounded && flags.grounded { (-vy_before).max(0.0) } else { 0.0 };
                let dash_ended = prev.dashing && !flags.dashing;
                edges.landing_speed = edges.landing_speed.max(landed_speed);
                edges.dash_ended |= dash_ended;
                self.prev_flags = flags;

                // camera last: it needs this tick's edges
                let mut cam_in = CameraInput::from_character(&p.ch, &self.aim);
                cam_in.landed_speed = landed_speed;
                cam_in.dash_ended = dash_ended;
                self.pose = self.camera.update_aim(TICK_DT, cam_in, &self.aim);
            }
            None => {
                self.pending.push_back(intent);
                while self.pending.len() > 128 {
                    self.pending.pop_front();
                }
                self.pose = self.camera.update_aim(TICK_DT, CameraInput::default(), &self.aim);
            }
        }
        self.net.next_packet(intent, local_tick)
    }

    // ---- render ----

    pub fn view(&mut self, local_tick: f64) -> RenderView {
        let own_id = self.net.player_id();
        let remotes = match (self.net.render_tick(local_tick), self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t))) {
            (Some(rt), Some(newest)) => newest
                .entities
                .iter()
                .filter(|(id, _)| Some(*id) != own_id && *id < PROJECTILE_ID_BASE)
                .filter_map(|(id, _)| self.net.interp.sample(*id, rt).map(|s| (*id, s)))
                .collect(),
            _ => Vec::new(),
        };
        // other players' shurikens ride in the snapshot; our own are drawn from the prediction (no latency)
        let mut discs: Vec<([f32; 3], [f32; 3])> = self.pred.as_ref().map_or_else(Vec::new, |p| p.combat.projectiles());
        let mut disc_owners: Vec<u16> = vec![own_id.unwrap_or(0); discs.len()];
        if let (Some(rt), Some(newest)) = (self.net.render_tick(local_tick), self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t))) {
            for (id, st) in newest.entities.iter().filter(|(id, st)| *id >= PROJECTILE_ID_BASE && Some(st.yaw) != own_id) {
                if let Some(s) = self.net.interp.sample(*id, rt) {
                    let v = [dq_vel(st.vel[0]), dq_vel(st.vel[1]), dq_vel(st.vel[2])];
                    discs.push((s.pos, v));
                    disc_owners.push(st.yaw);
                }
            }
        }
        let off = self.reconciler.visual_offset;
        let local_pos = match &self.pred {
            Some(p) => {
                let a = (local_tick - p.sim_local).clamp(0.0, 1.0) as f32;
                let l = |i: usize| p.prev_pos[i] + (p.pos[i] - p.prev_pos[i]) * a + off[i];
                Some([l(0), l(1), l(2)])
            }
            None => own_id
                .zip(self.net.render_tick(local_tick))
                .and_then(|(id, rt)| self.net.interp.sample(id, rt))
                .map(|s| s.pos),
        };
        let mut fx = std::mem::take(&mut self.fx_edges);
        if let Some(p) = &self.pred {
            fx.grounded = p.ch.state == MoveState::Grounded;
            fx.wall_running = matches!(p.ch.state, MoveState::WallRun { .. });
            fx.dashing = p.ch.evading().is_some();
            fx.rolling = p.ch.evading() == Some(true);
            fx.sliding = p.ch.sliding();
            fx.speed = (p.ch.vel.x * p.ch.vel.x + p.ch.vel.z * p.ch.vel.z).sqrt();
            fx.dash_ready = p.ch.dash_ready();
            fx.grappling = p.ch.grapple.is_some();
            fx.grapple_ready = !fx.grappling && p.ch.grapple_cooldown() <= 0.0;
            fx.grapple_cooldown = (p.ch.grapple_cooldown() / self.cfg.sim.grapple_cd.max(1e-3)).clamp(0.0, 1.0);
            fx.grapple_hookable = p.hookable;
        }
        if let Some(p) = &self.pred {
            fx.slice_cooldown = (p.ch.slice_cooldown() / p.katana.cooldown.max(1e-3)).clamp(0.0, 1.0);
        }
        fx.throw_cooldown = (self.throw_cd_left / ShurikenCfg::default().cooldown.max(1e-3)).clamp(0.0, 1.0);
        let (build_ghost, blocks) = match (self.pred.as_mut(), own_id) {
            (Some(p), Some(id)) => {
                let aim = CombatAim {
                    origin: vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]],
                    yaw: self.aim.yaw,
                    pitch: self.aim.pitch,
                };
                (Some(p.build_ghost(id, self.last_input_tick, self.build_sel, &aim)), p.blocks.pieces())
            }
            _ => (None, self.net.block_pieces()),
        };
        let lock = self.pred.as_ref().and_then(|p| {
            let l = &p.combat.lock;
            (l.phase != LockPhase::Search).then(|| LockOn {
                target: l.target_id as EntityId,
                pos: l.pos,
                progress: l.progress,
                locked: l.phase == LockPhase::Locked,
                phase: l.phase,
            })
        });
        let (camera, weapon) = self.pose;
        let eye = local_pos.map_or([0.0; 3], |p| [p[0], p[1] + self.cfg.sim.eye_above_center + camera.y_offset, p[2]]);
        RenderView {
            fx,
            feel: self.camera.feel,
            connect_failed: self.net.connect_failed(local_tick),
            hp: self.net.hp(),
            hits: self.net.take_hits(),
            board: self.net.board().cloned(),
            connected: own_id.is_some(),
            kicked: self.net.kicked(),
            camera,
            weapon,
            eye,
            local_pos,
            correction_offset: off,
            remotes,
            events: self.net.take_events(),
            build_ghost,
            lock,
            lock_edges: std::mem::take(&mut self.lock_edges),
            blocks,
            block_size: BlockCfg::default().size,
            block_thickness: BlockCfg::default().thickness,
            aim: self.aim,
            discs,
            disc_owners,
            rope: self.pred.as_ref().and_then(|p| p.ch.grapple).map(|g| (eye, [g.anchor.x, g.anchor.y, g.anchor.z])),
        }
    }

    // ---- introspection (tests, debug overlay) ----

    /// The touch stick as the next intent would read it (tests, debug overlay).
    pub fn touch_stick_for_test(&self) -> (f32, f32) {
        self.touch.stick()
    }

    /// Simulated (undrawn-offset) predicted centre, once prediction has started.
    pub fn predicted_pos(&self) -> Option<[f32; 3]> {
        self.pred.as_ref().map(|p| p.pos)
    }

    pub fn predicting(&self) -> bool {
        self.pred.is_some()
    }
}
