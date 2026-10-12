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
use shinobi_proto::quant::{dq_pos, dq_pos3, dq_vel};
use shinobi_proto::snapshot::{disc_kind, EntityId, DISC_ARMED, DISC_STUCK, POWER_ID_BASE, PROJECTILE_ID_BASE, STUCK_NORMAL_SPEED};
use shinobi_sim::power::{PowerCfg, PowerEvent, PowerInput, PowerKind, PowerState};
use crate::power_view::{PowerView, PowerWorld};
use shinobi_proto::TICK_DT;
use shinobi_sim::balance::FeedbackBalance;
use shinobi_sim::combat::aim_assist::{LockEdge, LockPhase};
use shinobi_sim::blocks::{BlockCfg, BlockEvent, BlockInfo, Blocks, Piece, PieceKind};
use shinobi_sim::character::{Cfg, CharacterSystem, GrappleEvent, MoveState, PhysicsWorld, PlayerInput};
use shinobi_sim::combat::katana::{predict_slice, KatanaCfg};
use shinobi_sim::combat::shuriken::ShurikenCfg;
use shinobi_sim::combat::stars::{within, StarCfg, StarKind};
use shinobi_sim::combat::{Aim as CombatAim, CombatEvent, CombatInput, CombatSystem, GROUP_HITBOX, GROUP_MELEE, GROUP_PLAYER, GROUP_PROJECTILE};
use shinobi_sim::events::NetEvent;
use shinobi_sim::level::LevelDesc;
use shinobi_sim::movement::{refresh_solid, step_player_with};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::{to_player_input, view_dir};

use crate::controls_card::{ControlRow, ControlsCard, Use};
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
    /// Held: the block / parry shield. The view is locked where it was when the hold began (look input is ignored) while
    /// movement, jump, dash, grapple and wall-run keep working; throwing and cutting are shut while it is up.
    Block,
    /// Cycle the throwable the throw button sends (mouse wheel / `R`): star, barrel, flash, canister. One-tick edges.
    StarNext,
    StarPrev,
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
    /// (the estimate lags the real server tick by one one-way trip), and stay under `MAX_FUTURE_TICKS` (24).
    pub lead_ticks: u32,
    /// Katana homing: a slice press with a lock turns the view onto the target (see `sim_tick`).
    pub melee_home: bool,
    /// Look sensitivity, radians per device unit.
    pub rad_per_unit: f32,
    /// Hit-stop after a katana hit / kill, seconds (Balance `feedback.hitstop_*`). Client-only: the sim never pauses.
    pub hitstop_slash: f32,
    pub hitstop_kill: f32,
    /// Look assist strengths (Balance `assist.*`): friction / magnetism while a target is in the lock cone.
    pub assist: shinobi_sim::combat::aim_assist::AssistCfg,
}

impl Default for ClientGameCfg {
    fn default() -> Self {
        Self {
            sim: Cfg::default(),
            camera: FpsCameraCfg::default(),
            lead_ticks: 8,
            melee_home: true,
            rad_per_unit: 0.0022,
            hitstop_slash: FeedbackBalance::default().hitstop_slash_ms / 1000.0,
            hitstop_kill: FeedbackBalance::default().hitstop_kill_ms / 1000.0,
            assist: Default::default(),
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
    /// The hook would grab a player (aim assist included): the crosshair turns red. Refreshed with `hookable`.
    hook_player: bool,
    /// The rope is on a player (hook key again = yank). Refreshed with `hookable`.
    rope_on_player: bool,
    /// The rope is an enemy hook (it pulls them, not us): the shell draws the 'hooked' effect.
    pulling_enemy: bool,
    /// Client clock (ticks) at the most recent `sim_tick`; interpolation alpha is measured from here.
    sim_local: f64,
    /// Predicted position per recent sim tick, `(client tick clock, pos)`, oldest first: the draw time can sit up to
    /// `display_clock::MAX_LAG` ticks behind the newest tick, so a single previous position is not enough to interpolate.
    hist: std::collections::VecDeque<(f64, [f32; 3])>,
    /// Replicated + predicted blocks as colliders in `world` (same code as the server: `shinobi_sim::blocks`).
    blocks: Blocks,
    /// Blocks placed locally and not yet confirmed by the server: `(cell, local tick placed)`.
    block_pending: Vec<([i32; 3], u32)>,
    /// Sensor hitboxes standing in for the other players (placed at where our screen shows them), so the
    /// predicted combat finds the same lock-on target the server will.
    /// `(sensor hitbox, solid movement hull)` per remote: the hull is what a lunge or a run bumps into on the
    /// server (other players' capsules are solid to the character controller, corpses included).
    proxies: HashMap<EntityId, (ColliderHandle, ColliderHandle)>,
    /// Destructible level boxes: the server's broken set is applied to `world` every tick (`breaks.rs`).
    breaks: shinobi_sim::breaks::LevelBreaks,
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
    fn sync_proxies(&mut self, cfg: &Cfg, remotes: &[(EntityId, [f32; 3], bool, bool)]) -> Vec<ColliderHandle> {
        let gone: Vec<EntityId> = self.proxies.keys().copied().filter(|id| !remotes.iter().any(|r| r.0 == *id)).collect();
        for id in gone {
            if let Some((hit, hull)) = self.proxies.remove(&id) {
                for h in [hit, hull] {
                    self.world.colliders.remove(h, &mut self.world.islands, &mut self.world.bodies, false);
                }
            }
        }
        let mut allies = Vec::new();
        for (id, pos, ally, dead) in remotes {
            let (h, hull) = *self.proxies.entry(*id).or_insert_with(|| {
                let hit = self.world.colliders.insert(
                    ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                        .sensor(true)
                        .collision_groups(InteractionGroups::new(GROUP_HITBOX, GROUP_PROJECTILE | GROUP_MELEE))
                        .user_data(*id as u128)
                        .build(),
                );
                // the server's player capsule: solid, in GROUP_PLAYER (shots and slashes ignore it, movement does not)
                let hull = self.world.colliders.insert(
                    ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).collision_groups(InteractionGroups::new(GROUP_PLAYER, Group::ALL)).build(),
                );
                (hit, hull)
            });
            let pose = Isometry::translation(pos[0], pos[1], pos[2]);
            if let Some(col) = self.world.colliders.get_mut(h) {
                col.set_position(pose);
                // the dead cannot be targeted (the server hides their hitbox from every attacker)
                col.set_collision_groups(if *dead { InteractionGroups::none() } else { InteractionGroups::new(GROUP_HITBOX, GROUP_PROJECTILE | GROUP_MELEE) });
            }
            if let Some(col) = self.world.colliders.get_mut(hull) {
                col.set_position(pose);
                // the server keeps a dead player's hull solid until they respawn (the capsule never leaves the world),
                // so the proxy stays solid too: flipping it on the death flag would reach us a round trip late, in the
                // middle of exactly the lunge that killed them
            }
            if *ally {
                allies.push(h);
            }
        }
        allies
    }

    /// The build press, run through the same `Blocks::try_build` the server uses.
    fn predict_build(&mut self, owner: u16, tick: u32, kind: PieceKind, kills: u32, aim: &CombatAim) {
        self.world.queries.update(&self.world.colliders);
        let f = aim.forward();
        let mut ev = Vec::new();
        let body = self.ch.body;
        // size scales with the owner's kills (rules + kills known to the client; the server's BlockPlaced corrects any drift)
        self.blocks.try_build(&mut self.world, tick, owner, kind, kills, [aim.origin.x, aim.origin.y, aim.origin.z], [f.x, f.y, f.z], body, &[self.pos], &mut ev);
        for e in ev {
            if let BlockEvent::Placed { cell, .. } = e {
                self.block_pending.push((cell, tick));
                self.world.queries.update(&self.world.colliders);
            }
        }
    }

    /// Where a press would place a block right now, and whether it would be accepted.
    fn build_ghost(&mut self, owner: u16, tick: u32, kind: PieceKind, kills: u32, aim: &CombatAim) -> BuildGhost {
        self.world.queries.update(&self.world.colliders);
        let f = aim.forward();
        let cell = self.blocks.target_cell(&self.world, [aim.origin.x, aim.origin.y, aim.origin.z], [f.x, f.y, f.z], self.ch.body);
        let valid = self.blocks.is_free(cell) && !self.blocks.overlaps_player(cell, &[self.pos]);
        let piece = Piece::for_press(kind, [f.x, f.y, f.z]);
        let slab = self.blocks.slab(cell, piece, self.blocks.cfg.scale_q(kills));
        BuildGhost { cell, piece, center: slab.center, half: slab.half, rot: slab.rot, valid, ready: self.blocks.ready(owner, tick) }
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

    /// Predicted position at client time `t` (ticks): linear between the bracketing ticks, held outside the history.
    fn sample(&self, t: f64) -> [f32; 3] {
        let h = &self.hist;
        let (Some(first), Some(last)) = (h.front(), h.back()) else { return self.pos };
        if t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        for w in h.iter().collect::<Vec<_>>().windows(2) {
            if t <= w[1].0 {
                let a = ((t - w[0].0) / (w[1].0 - w[0].0).max(1e-9)) as f32;
                return std::array::from_fn(|i| w[0].1[i] + (w[1].1[i] - w[0].1[i]) * a);
            }
        }
        last.1
    }

    /// Can the grapple grab something along this view direction from here? (Crosshair tint.)
    fn probe_hook(&mut self, aim_yaw: u16, aim_pitch: u16) {
        refresh_solid(&mut self.solid, &self.world.colliders);
        std::mem::swap(&mut self.world.queries, &mut self.solid);
        let eye = vector![self.pos[0], self.pos[1] + self.ch.eye_above(), self.pos[2]];
        let target = self.ch.grapple_target_kind(&self.world, eye, view_dir(aim_yaw, aim_pitch));
        self.hookable = target.is_some();
        self.hook_player = target.map_or(false, |t| t.2);
        self.rope_on_player = self.ch.rope_on_player(&self.world);
        self.pulling_enemy = self.ch.pulling_enemy();
        std::mem::swap(&mut self.world.queries, &mut self.solid);
    }
}

/// The tick's `PlayerInput` as the server will build it: stick + buttons, plus the katana lunge decision made
/// from the (restored or live) cooldown mirror in `ch`. Shared by the catch-up and the replay hook.
fn lunge_input(katana: &KatanaCfg, it: &PlayerIntent, ch: &mut CharacterSystem) -> PlayerInput {
    let mut input = to_player_input(it);
    let mut cd = ch.slice_cooldown();
    let aim = CombatAim { origin: vector![0.0, 0.0, 0.0], yaw: dq_yaw(it.aim_yaw), pitch: 0.0 };
    predict_slice(katana.cooldown, &mut cd, TICK_DT, it.buttons.has(Buttons::SLICE), ch.dash_ready(), ch.strike_kind(), aim.flat_forward(), &mut input);
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
    /// Which katana move this was (light / heavy / air / whirl / counter).
    pub kind: shinobi_sim::combat::SlashKind,
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
    /// Heavy-cut charge 0..1 (the katana button held after a cut).
    pub slice_charge: f32,
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
    /// ... and what it would grab is a player (the crosshair turns red).
    pub grapple_target_player: bool,
    /// The rope is on an enemy and pulls THEM toward us (`CharacterSystem::pulling_enemy`): the 'hooked' effect.
    pub grapple_pulling_enemy: bool,
    /// A katana swing right now is a strike: 0 none, 1 hook-strike (just released), 2 wall slash (wall-run), 3 hook-strike
    /// + rope slash (rope taut), 4 the same with the rope on a player (hook key again = yank).
    pub strike_armed: u8,
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
    /// `progress` with smoothstep easing: animate the dots with this, not the raw per-tick value.
    pub eased: f32,
    /// Seconds since the phase changed (or a new target was acquired): drives snap pulses and scatter timing.
    pub t_since_phase: f32,
    /// Target velocity (m/s, smoothed): lead the dots / brackets along it between ticks.
    pub target_vel: [f32; 3],
    /// 0..=1 weight the assist has right now (ramps in over ~0.15 s, out over the break time).
    pub blend: f32,
}

/// Block placement preview for the shell: draw a translucent cube at `center` (cyan when `valid`, red otherwise);
/// `ready` is false during the placement cooldown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuildGhost {
    pub cell: [i32; 3],
    pub piece: Piece,
    /// Slab centre, half extents (draw a box of 2 * half at center) and scaled-axis rotation (ramps are tilted).
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub rot: [f32; 3],
    pub valid: bool,
    pub ready: bool,
}

/// The local player is dead and waiting to respawn (set from the server's dead flag until it clears).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeathView {
    /// Seconds since the death was seen.
    pub t: f32,
    /// Respawn delay from the server's `MatchRules`, seconds (a default until the join catch-up arrives).
    pub total: f32,
    /// Where the body fell (centre): the shell bursts it into fragments here.
    pub pos: [f32; 3],
    pub killer: Option<EntityId>,
    /// True only on the first view after dying: the shell spawns the one-shot effects.
    pub fresh: bool,
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
    /// Ids in `remotes` with an unobstructed line from the eye to the chest (cached ~100 ms): HUD tags hide behind walls.
    pub los_ids: Vec<EntityId>,
    /// Reliable events (walls, obstacles) received since the previous view.
    pub events: Vec<NetEvent>,
    pub aim: Aim,
    /// Shurikens in flight: world position, direction (own from prediction, others from the snapshot).
    pub discs: Vec<([f32; 3], [f32; 3])>,
    /// Owner player id of each entry in `discs` (same index), for per-player colours. 0 = unknown.
    pub disc_owners: Vec<u16>,
    /// Own wrist-grapple rope as (hand/eye, anchor) while attached, from the prediction.
    pub rope: Option<([f32; 3], [f32; 3])>,
    /// First-30-seconds controls card rows still showing (key, label, opacity).
    pub controls: Vec<ControlRow>,
    /// Named district under the local player (HUD compass / radar zone label, per-district tint); `None` before the first position.
    pub district: Option<shinobi_sim::level::District>,
    /// Placement preview for the block under the crosshair (None before prediction starts or while dead).
    pub build_ghost: Option<BuildGhost>,
    /// Set while we are dead: camera, controls and HUD switch to the death sequence.
    pub death: Option<DeathView>,
    /// The shield is up (the block key is held and we are alive): the HUD draws the guard ring.
    pub blocking: bool,
    /// Star energy (charges, pool size, locked out): the client's estimate, synced to the server's. `None` = no pool.
    pub star: Option<(f32, f32, bool)>,
    /// The throwable the throw button sends now, whether the player ever cycled it, and plain throws made so far
    /// (the "WHEEL / R switches star" hint keys off the last two).
    pub star_sel: StarKind,
    pub star_cycled: bool,
    pub star_throws: u32,
    /// Landed barrels and canisters (all players'), and the kind of each flying disc in `discs`.
    pub star_objs: Vec<StarObj>,
    pub disc_kinds: Vec<StarKind>,
    /// Flash white-out right now, 0..=1 (screen flash strength, `whiteout_level` over the server's blind time).
    pub blind: f32,
    /// Fraction of the daze left, 1 -> 0 (0 = not dazed): the HUD's DAZED cue and the ears' whine follow it.
    pub dazed: f32,
    /// Hidden enemies (currently always empty).
    pub hidden_ids: Vec<EntityId>,
    /// How far the throw button has charged toward a MARK star, 0..=1 (0 when up or after a tap).
    pub charging: f32,
    /// A throw was refused this tick for lack of energy.
    pub star_refused: bool,
    /// Predator lock-on (docs/lock-on-and-hud.md): `None` while searching, else the target as the predictor sees it.
    pub lock: Option<LockOn>,
    /// Lock-on edges since the previous view, in order, for audio / flashes to trigger on.
    pub lock_edges: Vec<LockEdge>,
    /// The power weapon in hand (docs/powerups.md), predicted; `None` with empty hands and no tags out.
    pub power: Option<PowerView>,
    /// Arrows, fuda tags and drawn bows in the newest snapshot (everyone's, ours included).
    pub power_world: PowerWorld,
    /// Our own power-weapon moments since the previous view (bow drawn, arrow loosed, tag thrown...), for pose and sound.
    pub power_events: Vec<PowerEvent>,
    /// Every live block as `(owner, cell)`; centre = `(cell + 0.5) * block_size`. Server-confirmed plus our own pending.
    pub blocks: Vec<BlockInfo>,
    /// Edge length of a block cell, metres. Slab geometry: `block_geometry`.
    pub block_size: f32,
    /// Slab thickness, metres.
    pub block_thickness: f32,
}

/// Share of a flash daze that is solid white before the screen starts to clear.
const WHITEOUT_HOLD: f32 = 0.25;

/// White-out strength factor 0..=1 for `frac_left` of the daze remaining (1 = just hit): solid for the first quarter,
/// then a smooth fade to nothing exactly when the daze ends.
pub fn whiteout_level(frac_left: f32) -> f32 {
    let k = (frac_left.clamp(0.0, 1.0) / (1.0 - WHITEOUT_HOLD)).min(1.0);
    k * k * (3.0 - 2.0 * k)
}

/// A landed star object (barrel or canister) for the renderer.
#[derive(Clone, Copy, Debug)]
pub struct StarObj {
    pub kind: StarKind,
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub owner: u16,
    /// Explosives: the fuse is burning (landed).
    pub armed: bool,
    /// Fraction of its life left, 1 = fresh.
    pub life_left: f32,
}

impl RenderView {
    /// (yaw, pitch) the camera must render with: exactly the aim that goes out in intents, so the crosshair is
    /// where the shot goes. Recoil, bob, roll and the FX look-kick are NOT added here (roll / FOV are separate
    /// pose channels; the kick was dropped from the view because it moved the crosshair off the real aim).
    pub fn look_angles(&self) -> (f32, f32) {
        (self.aim.yaw, self.aim.pitch)
    }
}

/// Katana homing (see `sim_tick`): range (m) and half-cone (rad) in which a slice press turns the view onto the lock,
/// the range / per-tick step of the slow tracking turn during the swing cadence.
/// Star energy as the client estimates it between server reports (the server's pool is authoritative): seconds per
/// recovered charge and the charges needed to throw again after running dry; the cost of a charged MARK star; sim ticks
/// the throw button must be held to charge one.
const STAR_REGEN_S: f32 = 0.45;
const STAR_RESUME: f32 = 2.0;
const MARK_COST: f32 = 2.0;
pub const MARK_CHARGE_TICKS: u32 = 24;
/// A throw-button hold up to this many ticks with a bow in the hands is a tap (still a star); `YumiCfg::tap_s` at 60 Hz.
const YUMI_TAP_TICKS: u32 = 10;
/// Ticks a thrown tag may be missing from the snapshot before the client stops counting it as live.
const TAG_SNAPSHOT_GRACE: u32 = 24;

const HOME_RANGE: f32 = 9.0;
const HOME_CONE: f32 = 0.9;
const HOME_TRACK_RANGE: f32 = 5.0;
const HOME_TRACK_STEP: f32 = 0.04;

pub struct ClientGame {
    pub cfg: ClientGameCfg,
    pub net: NetClient,
    pub aim: Aim,
    /// Look assist state (previous target angles) and whether the last look input came from touch.
    look_assist: shinobi_sim::combat::aim_assist::AimAssist,
    look_touch: bool,
    pub settings: Settings,
    screen: (f32, f32),
    pub reconciler: Reconciler,
    pub camera: FpsCamera,
    level: LevelDesc,
    latch: ButtonLatch,
    zip_edge: bool,
    /// The block key is down (raw), and whether the shield is actually up this tick (not while dead).
    block_held: bool,
    blocking: bool,
    /// Throw button held for this many sim ticks (None = up). The throw happens on RELEASE: a tap is a normal star, a hold
    /// of `MARK_CHARGE_TICKS` or more is a MARK star.
    fire_hold: Option<u32>,
    /// A release waiting for the next sim tick: `Some(true)` = charged (mark).
    fire_release: Option<bool>,
    /// The star-energy estimate (charges, pool size, locked out) and the last server report it was synced to.
    star_est: Option<(f32, f32, bool)>,
    star_seen: Option<(f32, f32, bool)>,
    /// This tick a throw was refused for lack of energy (the HUD flashes the empty readout).
    star_refused: bool,
    /// Selected throwable (mouse wheel / R), whether it was ever changed, plain throws so far.
    star_sel: StarKind,
    star_cycled: bool,
    star_throws: u32,
    /// White-out from a flash star: ticks left / at start, and its strength.
    blind_left: u32,
    blind_total: u32,
    blind_strength: f32,
    /// Hidden enemies as of the last tick (currently none).
    hidden_ids: Vec<EntityId>,
    /// Pending build press for the next tick: `Some(kind)` (latched until the tick, so a sub-tick tap is not lost).
    build_edge: Option<PieceKind>,
    /// Direction of the newest predicted katana swing (the hit kick opposes it).
    last_slash_dir: Option<[f32; 3]>,
    /// The first-30-seconds controls card.
    controls: ControlsCard,
    /// Piece the ghost previews: the one last printed (floor until the first press).
    build_sel: PieceKind,
    /// What `N` prints: floor, or ramp after a quick second tap (during the placement cooldown).
    n_variant: PieceKind,
    /// Tick of the last `N` tap that printed (a tap within the cooldown of it cycles the variant instead).
    n_press_tick: Option<u32>,
    /// Tick of the newest intent sent (cooldown readiness for the ghost).
    last_input_tick: u32,
    /// Lock-on edges (acquire / lock / break) since the previous `view()`.
    lock_edges: Vec<LockEdge>,
    touch: TouchController,
    pred: Option<Predicted>,
    /// Remote players with a clear line from our eye to their chest (level colliders only), refreshed every ~100 ms.
    los_cache: (f64, Vec<EntityId>),
    /// Intents sent before the predictor has an authoritative starting state; replayed once it has.
    pending: VecDeque<PlayerIntent>,
    pose: (CameraPose, WeaponPose),
    /// The pose of the tick before, and the tick time the current one belongs to (`local_tick + 1`): `view` blends the
    /// two so roll / bob / FOV / hit-shake glide between 60 Hz ticks instead of stepping.
    pose_prev: (CameraPose, WeaponPose),
    pose_t: f64,
    /// Edge FX accumulated across sim ticks since the last `view()` (replays never add to it).
    fx_edges: LocalFx,
    throw_cd_left: f32,
    prev_flags: MoveFlags,
    /// The power weapon in hand: the predicted copy of the server's `Player::power` (same code, `power.rs`), the last
    /// (kind, ammo) report it was synced to, the sim ticks of tags thrown that the snapshot may not show yet, and the
    /// moments since the last view. `fire_release_len` = ticks the throw button had been down at its release.
    power: PowerState,
    power_seen: Option<(u8, u8)>,
    tag_throws: Vec<u32>,
    power_events: Vec<PowerEvent>,
    fire_release_len: u32,
    /// Number of corrections applied (replays), for stats / debug overlay.
    pub corrections: u32,
    /// The local death in progress (see `death_cam`).
    death: Option<Death>,
    /// Newest kill against us seen before the death state existed: (killer, local tick).
    last_kill: Option<(EntityId, f64)>,
}

/// Captured when the dead flag first arrives.
struct Death {
    t0: f64,
    pos: [f32; 3],
    yaw0: f32,
    pitch0: f32,
    killer: Option<EntityId>,
    fresh: bool,
}

impl ClientGame {
    pub fn new(level: LevelDesc, mut cfg: ClientGameCfg) -> Self {
        cfg.camera.slide_drop = cfg.sim.slide_eye_drop; // one source for the eye drop: camera and shot origin agree
        Self {
            camera: FpsCamera::new(cfg.camera),
            cfg,
            net: NetClient::default(),
            aim: Aim::default(),
            look_assist: Default::default(),
            look_touch: false,
            settings: Settings::default(),
            screen: (1280.0, 720.0),
            reconciler: Reconciler::default(),
            level,
            latch: ButtonLatch::default(),
            zip_edge: false,
            block_held: false,
            blocking: false,
            fire_hold: None,
            fire_release: None,
            star_est: None,
            star_seen: None,
            star_refused: false,
            star_sel: StarKind::Standard,
            star_cycled: false,
            star_throws: 0,
            blind_left: 0,
            blind_total: 0,
            blind_strength: 0.0,
            hidden_ids: Vec::new(),
            build_edge: None,
            last_slash_dir: None,
            controls: ControlsCard::default(),
            build_sel: PieceKind::Floor,
            n_variant: PieceKind::Floor,
            n_press_tick: None,
            last_input_tick: 0,
            lock_edges: Vec::new(),
            touch: TouchController::new(1280.0, 90.0),
            pred: None,
            los_cache: (f64::NEG_INFINITY, Vec::new()),
            pending: VecDeque::new(),
            pose: (CameraPose::default(), WeaponPose::default()),
            pose_prev: (CameraPose::default(), WeaponPose::default()),
            pose_t: f64::NEG_INFINITY,
            fx_edges: LocalFx::default(),
            throw_cd_left: 0.0,
            prev_flags: MoveFlags::default(),
            power: PowerState::default(),
            power_seen: None,
            tag_throws: Vec::new(),
            power_events: Vec::new(),
            fire_release_len: 0,
            corrections: 0,
            death: None,
            last_kill: None,
        }
    }

    // ---- input (any rate) ----

    /// F1: show / hide the controls strip (all rows, full strength) regardless of the 30 s fade.
    /// Offline review shots: pretend the shield is up (the sim tick that normally sets it does not run there).
    pub fn force_blocking(&mut self, on: bool) {
        self.blocking = on;
    }

    pub fn toggle_controls(&mut self) {
        self.controls.toggle_pin();
    }

    pub fn button_event(&mut self, e: ButtonEvent) {
        if e.down {
            match e.action {
                Action::Jump => self.controls.mark(Use::Jump),
                Action::Dash => self.controls.mark(Use::Dash),
                Action::Slide => self.controls.mark(Use::Slide),
                Action::Grapple => self.controls.mark(Use::Grapple),
                Action::BuildFloor | Action::BuildWall => self.controls.mark(Use::Build),
                Action::Slice => self.controls.mark(Use::Katana),
                Action::Fire => {
                    self.controls.mark(Use::Shuriken);
                    self.controls.mark(Use::Stars);
                }
                Action::Block => self.controls.mark(Use::Block),
                Action::StarNext | Action::StarPrev => self.controls.mark(Use::StarCycle),
                _ => {}
            }
        }
        match e.action {
            Action::Jump => self.latch.set_jump(e.down),
            Action::Dash => self.latch.set(Buttons::DASH, e.down),
            Action::Roll => self.latch.set(Buttons::ROLL, e.down),
            Action::Slice => self.latch.set(Buttons::SLICE, e.down),
            Action::Fire => {
                // the throw happens on release so a held press can become a charged MARK star
                if e.down {
                    self.fire_hold = Some(0);
                } else if let Some(h) = self.fire_hold.take() {
                    self.fire_release = Some(h >= MARK_CHARGE_TICKS);
                    self.fire_release_len = h;
                }
            }
            Action::Cling => self.latch.set(Buttons::WALL_CLING, e.down),
            Action::Slide => self.latch.set(Buttons::SLIDE, e.down),
            Action::Grapple => self.zip_edge |= e.down, // latched until the next sim tick, so a sub-tick tap is not lost
            Action::Block => self.block_held = e.down,
            Action::StarNext if e.down => {
                self.star_sel = self.star_sel.next();
                self.star_cycled = true;
            }
            Action::StarPrev if e.down => {
                self.star_sel = self.star_sel.prev();
                self.star_cycled = true;
            }
            Action::StarNext | Action::StarPrev => {}
            Action::BuildFloor if e.down => {
                let cd = self.pred.as_ref().map_or(BlockCfg::default().cooldown_ticks, |p| p.blocks.cfg.cooldown_ticks);
                let again = self.n_press_tick.map_or(false, |t| self.last_input_tick.wrapping_sub(t) < cd);
                if again {
                    // N again while the last print is still cooling down: switch floor <-> ramp, print nothing
                    self.n_variant = if self.n_variant == PieceKind::Floor { PieceKind::Ramp } else { PieceKind::Floor };
                } else {
                    self.build_edge = Some(self.n_variant);
                    self.n_press_tick = Some(self.last_input_tick);
                }
                self.build_sel = self.n_variant;
            }
            Action::BuildWall if e.down => {
                self.build_edge = Some(PieceKind::Wall);
                self.build_sel = PieceKind::Wall;
            }
            Action::BuildFloor | Action::BuildWall => {}
        }
    }

    /// Print a specific piece now (radial build menu): the same one-tick build edge a key press produces, with the
    /// N variant (floor / ramp) set to the choice. Latched until the next sim tick like any press.
    pub fn print_piece(&mut self, kind: PieceKind) {
        self.controls.mark(Use::Build);
        if kind != PieceKind::Wall {
            self.n_variant = kind;
            self.n_press_tick = Some(self.last_input_tick);
        }
        self.build_edge = Some(kind);
        self.build_sel = kind;
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
            pred.hist.clear();
            pred.hist.push_back((pred.sim_local + 1.0, p));
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
        let level_handles = self.level.insert_into(&mut world);
        let breaks = shinobi_sim::breaks::LevelBreaks::build(&self.level, &level_handles);
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
            hook_player: false,
            rope_on_player: false,
            pulling_enemy: false,
            sim_local: 0.0,
            hist: std::collections::VecDeque::new(),
            // the server evicts at its cap and says so (BlockRemoved): never evict locally on our own
            blocks: Blocks::new(BlockCfg { max_per_owner: usize::MAX, ..BlockCfg::default() }),
            block_pending: Vec::new(),
            proxies: HashMap::new(),
            breaks,
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
    /// Is the local player dead? From the newest snapshot's dead flag (the same source the HUD uses).
    fn own_dead(&self) -> bool {
        use shinobi_proto::snapshot::FLAG_DEAD;
        self.net
            .player_id()
            .zip(self.net.interp.newest_tick())
            .and_then(|(id, t)| self.net.interp.get(t)?.get(id).copied())
            .map_or(false, |e| e.flags & FLAG_DEAD != 0)
    }

    /// Start / end the death sequence from the dead flag; true while dead.
    fn update_death(&mut self, local_tick: f64) -> bool {
        let dead = self.own_dead();
        match (dead, self.death.is_some()) {
            (true, false) => {
                let own = self.net.player_id().zip(self.net.interp.newest_tick()).and_then(|(id, t)| self.net.interp.get(t)?.get(id).map(|e| e.pos));
                let pos = match &self.pred {
                    Some(p) => {
                        let o = self.reconciler.visual_offset;
                        Some([p.pos[0] + o[0], p.pos[1] + o[1], p.pos[2] + o[2]])
                    }
                    None => own.map(|q| [dq_pos(q[0]), dq_pos(q[1]), dq_pos(q[2])]),
                };
                let killer = self.last_kill.take().filter(|(_, t)| local_tick - *t < 120.0).map(|(id, _)| id);
                self.death = pos.map(|pos| Death { t0: local_tick, pos, yaw0: self.aim.yaw, pitch0: self.aim.pitch, killer, fresh: true });
            }
            (false, true) => self.death = None,
            _ => {}
        }
        dead
    }

    pub fn sim_tick(&mut self, frame: &InputFrame, local_tick: f64) -> Option<Vec<u8>> {
        for e in &frame.buttons {
            self.button_event(*e);
        }
        let aim_before = (self.aim.yaw, self.aim.pitch);
        let (dx, dy) = frame.look;
        // block: the view is frozen where it was when the shield went up (the server applies no other aim while it is up)
        let blocking = self.block_held && self.death.is_none();
        // a drawn bow slows the turn (docs/powerups.md: `combat.yumi.turn_mult`)
        let turn_k = if self.power.drawing { PowerCfg::default().yumi.turn_mult } else { 1.0 };
        // a flash-dazed player turns slowly (`combat.star.flash_daze_turn`); the server does not clamp it, the aim is the client's
        let turn_k = if self.blind_left > 0 { turn_k * StarCfg::default().flash_daze_turn } else { turn_k };
        let (dx, dy) = if blocking { (0.0, 0.0) } else { (dx * turn_k, dy * turn_k) };
        if dx != 0.0 || dy != 0.0 {
            self.settings.look(&mut self.aim, dx, dy, false);
            self.look_touch = false;
        }
        let (tx, ty) = self.touch.take_look();
        let (tx, ty) = if blocking { (0.0, 0.0) } else { (tx * turn_k, ty * turn_k) };
        if tx != 0.0 || ty != 0.0 {
            self.settings.look(&mut self.aim, tx, ty, true); // the aim pad has its own sensitivity
            self.look_touch = true;
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
        if self.pred.is_some() {
            self.controls.tick(TICK_DT);
            if stick != (0.0, 0.0) {
                self.controls.mark(Use::Move);
            }
        }
        // dead: the server ignores us and the body is gone, so nothing we press may move, fire or aim anything
        let dead = self.update_death(local_tick);
        if dead {
            self.aim.yaw = aim_before.0;
            self.aim.pitch = aim_before.1;
        }
        // look assist: slow the look and follow the target's motion while one is locked. It edits the aim BEFORE the
        // intent is built, so the server sees exactly the aim the player (and the predictor) used.
        let assist_in = self.pred.as_ref().filter(|p| !dead && !blocking && p.combat.lock.pull(&p.combat.lock_cfg) > 0.0).map(|p| shinobi_sim::combat::aim_assist::AssistInput {
            eye: vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]],
            target: { let t = p.combat.lock.pos_now(); vector![t[0], t[1], t[2]] },
            target_id: p.combat.lock.target_id,
            pull: p.combat.lock.pull(&p.combat.lock_cfg),
            cone_deg: p.combat.assist_cone_deg,
            touch: self.look_touch,
            dt: TICK_DT,
        });
        let (ay, ap) = self.look_assist.apply(&self.cfg.assist, assist_in.as_ref(), aim_before, (self.aim.yaw, self.aim.pitch));
        self.aim.yaw = ay;
        self.aim.pitch = ap;
        // a released throw button becomes a FIRE edge (and the MARK flag when it was held long enough)
        let mut mark_now = false;
        if let Some(h) = &mut self.fire_hold {
            *h += 1;
        }
        if let Some(m) = self.fire_release.take() {
            // a bow in the hands: the held button WAS the draw. A release after a real draw is the shot (or its cancel), no
            // throw; only a tap still throws the star. Tags take every press as theirs and never charge a MARK star.
            let bow_draw = self.power.kind == PowerKind::Yumi && self.fire_release_len > YUMI_TAP_TICKS;
            if !bow_draw {
                self.latch.set(Buttons::FIRE, true);
                self.latch.set(Buttons::FIRE, false);
                mark_now = m && self.power.kind == PowerKind::None;
            }
        }
        // the weapon the server says we hold (re-synced when its kind / ammo report changes, never on a stale repeat)
        match self.net.power_report() {
            Some((k, a, l)) => {
                if self.power_seen != Some((k, a)) {
                    self.power_seen = Some((k, a));
                    let kind = PowerKind::from_u8(k);
                    if kind == PowerKind::None {
                        self.power.clear();
                    } else if self.power.kind != kind {
                        self.power.clear();
                        self.power.grant(kind, a, l as f32 / 4.0, &PowerCfg::default());
                    } else {
                        self.power.ammo = a;
                    }
                }
                if self.power.held() && (self.power.left - l as f32 / 4.0).abs() > 1.0 {
                    self.power.left = l as f32 / 4.0;
                }
            }
            None => {}
        }
        // our own tags that exist (snapshot) or were thrown a moment ago (the snapshot has not caught up yet)
        let me_id = self.net.player_id().unwrap_or(0);
        let (snap_live, snap_armed) = self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t)).map_or((0, 0), |s| PowerWorld::of(s, |_| None).tags_of(me_id));
        self.tag_throws.retain(|t| tick.wrapping_sub(*t) < TAG_SNAPSHOT_GRACE);
        let live_tags = snap_live.max(self.tag_throws.len() as u32);
        let tag_press = self.power.kind == PowerKind::Tag && (self.power.ammo > 0 || live_tags > 0);
        let mut held = self.latch.sample();
        let blocking = blocking && !dead;
        if blocking {
            // a raised shield shuts throwing and cutting (the server drops them too), dash / jump / grapple stay live
            held = Buttons(held.0 & !(Buttons::FIRE | Buttons::SLICE));
        }
        // star energy: follow the server's pool (re-synced whenever it reports), regenerate in between, and do not throw
        // (or play a throw) a star the server would refuse
        self.star_refused = false;
        match self.net.star_energy() {
            None => self.star_est = None,
            Some(srv) => {
                if self.star_seen != Some(srv) {
                    self.star_seen = Some(srv);
                    self.star_est = Some(srv);
                }
                if let Some((e, max, locked)) = &mut self.star_est {
                    *e = (*e + TICK_DT / STAR_REGEN_S).min(*max);
                    if *locked && *e + 1e-3 >= STAR_RESUME.min(*max) {
                        *locked = false;
                    }
                }
            }
        }
        if held.has(Buttons::FIRE) && tag_press {
            mark_now = false; // the press belongs to the tags, no star is spent on it
        } else if held.has(Buttons::FIRE) {
            if let Some((e, _, locked)) = &mut self.star_est {
                let cost = if mark_now { MARK_COST } else { StarCfg::default().cost(self.star_sel, 1.0) };
                if *locked || *e + 1e-3 < cost {
                    held = Buttons(held.0 & !Buttons::FIRE);
                    mark_now = false;
                    self.star_refused = true;
                } else {
                    *e -= cost;
                    if *e + 1e-3 < 1.0 {
                        *locked = true;
                    }
                }
            }
        } else {
            mark_now = false;
        }
        // melee homing: the lunge goes where the AIM points (client prediction and server both use it), so the aim is
        // what gets bent. Pressing slice with a lock turns the view onto the locked target (within range and a generous
        // cone); in the swing cadence after it the view keeps turning toward the target, slowly, and only while the
        // player is not turning it themselves, so it never fights the look.
        if !dead && !blocking && self.cfg.melee_home {
            if let Some(p) = &self.pred {
                if p.combat.lock.holding() {
                    let eye = vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]];
                    let t = p.combat.lock.pos_now();
                    let to = vector![t[0], t[1], t[2]] - eye;
                    let dist = (to.x * to.x + to.z * to.z).sqrt();
                    let (ty, _) = shinobi_sim::combat::aim_assist::angles_of(vector![to.x, 0.0, to.z]);
                    let err = (ty - self.aim.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                    let cooling = p.combat.slice_cooldown_left() > 0.0;
                    if held.has(Buttons::SLICE) && dist <= HOME_RANGE && err.abs() <= HOME_CONE {
                        self.aim.yaw = ty.rem_euclid(std::f32::consts::TAU);
                    } else if cooling && dist <= HOME_TRACK_RANGE && err.abs() <= HOME_CONE && frame.look == (0.0, 0.0) {
                        self.aim.yaw = (self.aim.yaw + err.clamp(-HOME_TRACK_STEP, HOME_TRACK_STEP)).rem_euclid(std::f32::consts::TAU);
                    }
                }
            }
        }
        let mut intent = if dead { build_intent(tick, (0.0, 0.0), &self.aim, Buttons::default()) } else { build_intent(tick, stick, &self.aim, held) };
        intent.zip = std::mem::take(&mut self.zip_edge) && !dead;
        intent.block = blocking;
        intent.mark = mark_now;
        // the throwable a plain press sends rides with the FIRE edge (a MARK star ignores it)
        intent.star = if intent.buttons.has(Buttons::FIRE) && !mark_now { self.star_sel as u8 } else { 0 };
        if intent.buttons.has(Buttons::FIRE) && !mark_now && !tag_press {
            self.star_throws += 1;
        }
        // the bow: the held throw button is the draw (the server also needs it to be a yumi, and slows the walk by it)
        intent.draw = self.fire_hold.is_some() && self.power.kind == PowerKind::Yumi && !blocking && !dead;
        // the weapon's own state machine, with exactly the inputs the server gives its copy
        if dead {
            self.power.clear();
        } else {
            let mut pev = Vec::new();
            let armed = snap_armed;
            self.power.update(TICK_DT, &PowerInput { draw: intent.draw, fire: intent.buttons.has(Buttons::FIRE), live_tags, armed_tags: armed, can_act: !blocking }, &PowerCfg::default(), &mut pev);
            for e in pev {
                if e == PowerEvent::TagThrow {
                    self.tag_throws.push(tick);
                }
                self.power_events.push(e);
            }
        }
        let star_fire_ok = !self.power.blocks_star();
        self.blocking = blocking;
        // flash white-out from the server (events of this tick); it also keeps lock-on from forming in our prediction
        for (q, ticks) in self.net.take_blind() {
            if ticks as u32 >= self.blind_left {
                self.blind_left = ticks as u32;
                self.blind_total = (ticks as u32).max(1);
                self.blind_strength = q as f32 / 255.0;
            }
        }
        if dead {
            self.blind_left = 0;
        }
        let press = self.build_edge.take().filter(|_| !dead);
        intent.build = press.is_some();
        intent.build_wall = press == Some(PieceKind::Wall);
        intent.build_ramp = press == Some(PieceKind::Ramp);
        self.last_input_tick = tick;
        let own_id_for_blocks = self.net.player_id().unwrap_or(0);

        match &mut self.pred {
            Some(p) => {
                // blocks before anything moves: the world this tick collides with = the server's blocks + our own unconfirmed ones
                let my_id = own_id_for_blocks;
                if let Some(r) = self.net.block_rules() {
                    p.blocks.cfg = p.blocks.cfg.with_rules(&r);
                }
                p.sync_blocks(&self.net.block_pieces(), tick);
                // destructible level boxes the server has broken are gone from the predicted world too
                if p.breaks.set_broken(&mut p.world, self.net.world_broken()) {
                    p.world.queries.update(&p.world.colliders);
                }
                let my_kills = self.net.board().and_then(|b| b.rows.iter().find(|r| r.id as u16 == my_id).map(|r| r.kills as u32)).unwrap_or(0);
                if let Some(kind) = press {
                    let aim = CombatAim { origin: vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]], yaw: dq_yaw(intent.aim_yaw), pitch: dq_pitch(intent.aim_pitch) };
                    p.predict_build(my_id, tick, kind, my_kills, &aim);
                }
                // combat first, like the server: cosmetics + cooldown mirror (lunge is not applied, see Predicted)
                let mut ev = Vec::new();
                let mut sink = to_player_input(&intent);
                // the other players as our screen shows them: the lock-on (and the assisted throw) read these, so the
                // predictor finds the same target as the server's rewound hitboxes
                let own_id = self.net.player_id();
                let my_team = self.net.my_team();
                let remotes: Vec<(EntityId, [f32; 3], bool, bool)> = match (self.net.render_tick(local_tick), self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t))) {
                    (Some(rt), Some(newest)) => newest
                        .entities
                        .iter()
                        .filter(|(id, _)| Some(*id) != own_id && *id < PROJECTILE_ID_BASE)
                        .filter_map(|(id, st)| {
                            let ally = my_team.is_some() && self.net.team_of(*id) == my_team;
                            let dead = st.flags & shinobi_proto::snapshot::FLAG_DEAD != 0;
                            self.net.interp.sample(*id, rt).map(|s| (*id, s.pos, ally, dead))
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                p.combat.allies = p.sync_proxies(&self.cfg.sim, &remotes);
                // a white-out forms no lock at all (the server applies the same rule, so the predicted throw bends the same way)
                self.hidden_ids.clear();
                p.combat.hidden.clear();
                p.combat.suppress_lock = self.blind_left > 0;
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
                        fire: intent.buttons.has(Buttons::FIRE) && star_fire_ok,
                        slice: intent.buttons.has(Buttons::SLICE),
                        ultimate: intent.ultimate,
                        mark: intent.mark,
                        star: StarKind::from_u8(intent.star),
                    },
                    &aim,
                    &p.world,
                    &p.ch,
                    &mut sink,
                    &mut ev,
                );
                p.ch.set_slice_cooldown(p.combat.slice_cooldown_left()); // same mirror the server publishes
                // a landed barrel / canister is the server's from here on (it can burst or be shot): our copy only bridges the gap
                p.combat.drop_landed_stars(0.5);
                if let Some(edge) = p.combat.lock_edge {
                    self.lock_edges.push(edge);
                }
                let vy_before = p.ch.vel.y; // fall speed for the landing dip (the controller zeroes it on landing)
                let pos = p.step(&sink);
                p.sim_local = local_tick;
                p.hist.push_back((local_tick + 1.0, pos)); // the state after tick T is what is on screen at T + 1 (one tick of interpolation delay)
                while p.hist.len() > 8 {
                    p.hist.pop_front();
                }
                self.reconciler.record(intent, pos);
                p.probe_hook(intent.aim_yaw, intent.aim_pitch);
                match p.ch.grapple_event {
                    GrappleEvent::Attached { anchor, .. } => self.fx_edges.grapple_attached = Some([anchor.x, anchor.y, anchor.z]),
                    GrappleEvent::Released => self.fx_edges.grapple_released = true,
                    GrappleEvent::Yank { .. } => self.fx_edges.grapple_released = true,
                    GrappleEvent::Miss { .. } => self.fx_edges.grapple_missed = true,
                    GrappleEvent::None => {}
                }

                // cooldown mirror + cosmetic edges
                self.throw_cd_left = (self.throw_cd_left - TICK_DT).max(0.0);
                for e in ev {
                    match e {
                        CombatEvent::Slash { flip, dir, lunge, kind, .. } => {
                            self.fx_edges.slash = Some(PredictedSlash { lunge, dir: [dir.x, dir.y, dir.z], flip, kind });
                            self.last_slash_dir = Some([dir.x, dir.y, dir.z]);
                        }
                        CombatEvent::ShurikenThrown { dir, .. } => {
                            self.throw_cd_left = ShurikenCfg::default().cooldown;
                            self.fx_edges.threw = Some([dir.x, dir.y, dir.z]);
                        }
                        CombatEvent::SlashHit { point, killed, .. } => {
                            self.fx_edges.slash_hit = Some(PredictedSlashHit { point: [point.x, point.y, point.z], killed });
                            let stop = if killed { self.cfg.hitstop_kill } else { self.cfg.hitstop_slash };
                            self.fx_edges.hit_stop = self.fx_edges.hit_stop.max(stop);
                            // the camera reacts too (translation / roll / FOV only: the crosshair stays on the aim)
                            let right = {
                                let r = crate::camera::view_right(&self.aim);
                                self.last_slash_dir.map_or(0.0, |d| d[0] * r.x + d[2] * r.z)
                            };
                            self.camera.hit_react(right, killed, stop);
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
                self.pose_prev = self.pose;
                self.pose = self.camera.update_aim(TICK_DT, cam_in, &self.aim);
            }
            None => {
                self.pending.push_back(intent);
                while self.pending.len() > 128 {
                    self.pending.pop_front();
                }
                self.pose_prev = self.pose;
                self.pose = self.camera.update_aim(TICK_DT, CameraInput::default(), &self.aim);
            }
        }
        self.pose_t = local_tick + 1.0;
        self.blind_left = self.blind_left.saturating_sub(1);
        self.net.set_corrections(self.corrections);
        self.net.next_packet(intent, local_tick)
    }

    /// Landed barrels / canisters: our own predicted ones (until the snapshot brings the authoritative copy) plus every one in the
    /// newest snapshot. Pure of `self` borrows so the tick can call it while `pred` is held.
    fn star_objects_of(net: &NetClient, pred: Option<&Predicted>, own: Option<EntityId>) -> Vec<StarObj> {
        let sc = StarCfg::default();
        let mut out: Vec<StarObj> = Vec::new();
        if let (Some(p), Some(me)) = (pred, own) {
            for d in p.combat.loadout.discs().into_iter().filter(|d| d.stuck) {
                out.push(StarObj { kind: d.kind, pos: [d.pos.x, d.pos.y, d.pos.z], normal: [d.normal.x, d.normal.y, d.normal.z], owner: me, armed: d.armed(&sc), life_left: d.life_left(&sc) });
            }
        }
        if let Some(newest) = net.interp.newest_tick().and_then(|t| net.interp.get(t)) {
            for (_, st) in newest.entities.iter().filter(|(id, st)| *id >= PROJECTILE_ID_BASE && *id < POWER_ID_BASE && st.flags & DISC_STUCK != 0) {
                let (pos, kind) = (dq_pos3(st.pos), StarKind::from_u8(disc_kind(st.flags)));
                // our own copy of it (landed a moment ago, not yet dropped) already covers it
                if out.iter().any(|o| o.owner == st.yaw && o.kind == kind && within(o.pos, pos, 0.6)) {
                    out.retain(|o| !(o.owner == st.yaw && o.kind == kind && within(o.pos, pos, 0.6)));
                }
                let n = [dq_vel(st.vel[0]) / STUCK_NORMAL_SPEED, dq_vel(st.vel[1]) / STUCK_NORMAL_SPEED, dq_vel(st.vel[2]) / STUCK_NORMAL_SPEED];
                out.push(StarObj { kind, pos, normal: n, owner: st.yaw, armed: st.flags & DISC_ARMED != 0, life_left: (st.fx & 15) as f32 / 15.0 });
            }
        }
        out
    }

    /// Fraction of the current flash daze left (`None` = not dazed).
    fn blind_frac(&self) -> Option<f32> {
        (self.blind_left > 0 && self.blind_total > 0).then(|| (self.blind_left as f32 / self.blind_total as f32).min(1.0))
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
        let mut disc_kinds: Vec<StarKind> = self.pred.as_ref().map_or_else(Vec::new, |p| p.combat.loadout.discs().iter().filter(|d| !d.stuck).map(|d| d.kind).collect());
        disc_kinds.resize(discs.len(), StarKind::Standard);
        if let (Some(rt), Some(newest)) = (self.net.render_tick(local_tick), self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t))) {
            for (id, st) in newest.entities.iter().filter(|(id, st)| *id >= PROJECTILE_ID_BASE && *id < POWER_ID_BASE && st.flags & DISC_STUCK == 0 && Some(st.yaw) != own_id) {
                if let Some(s) = self.net.interp.sample(*id, rt) {
                    let v = [dq_vel(st.vel[0]), dq_vel(st.vel[1]), dq_vel(st.vel[2])];
                    discs.push((s.pos, v));
                    disc_owners.push(st.yaw);
                    disc_kinds.push(StarKind::from_u8(disc_kind(st.flags)));
                }
            }
        }
        // the offset decays once per tick; continue the decay between ticks so a correction glides out
        let since = ((local_tick - (self.pose_t - 1.0)) as f32).clamp(0.0, 1.0);
        let k = if self.pose_t.is_finite() { (-self.reconciler.decay_per_sec * TICK_DT * since).exp() } else { 1.0 };
        let off = self.reconciler.visual_offset.map(|v| v * k);
        let local_pos = match &self.pred {
            Some(p) => {
                let q = p.sample(local_tick);
                Some([q[0] + off[0], q[1] + off[1], q[2] + off[2]])
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
            fx.grapple_target_player = p.hookable && p.hook_player;
            fx.grapple_pulling_enemy = p.pulling_enemy;
            fx.strike_armed = match p.ch.strike_kind() {
                // 3 = the rope is taut (a swing is also a rope slash), 4 = and it is on a player (hook key again = yank)
                Some(shinobi_sim::character::StrikeKind::Hook) if p.ch.grapple.is_some() => 3 + p.rope_on_player as u8,
                Some(shinobi_sim::character::StrikeKind::Hook) => 1,
                Some(shinobi_sim::character::StrikeKind::Wall) => 2,
                None => 0,
            };
        }
        if let Some(p) = &self.pred {
            fx.slice_cooldown = (p.ch.slice_cooldown() / p.katana.cooldown.max(1e-3)).clamp(0.0, 1.0);
            fx.slice_charge = p.combat.slice_charge();
        }
        fx.throw_cooldown = (self.throw_cd_left / ShurikenCfg::default().cooldown.max(1e-3)).clamp(0.0, 1.0);
        let (build_ghost, blocks) = match (self.pred.as_mut(), own_id) {
            (Some(p), Some(id)) => {
                let aim = CombatAim {
                    origin: vector![p.pos[0], p.pos[1] + p.ch.eye_above(), p.pos[2]],
                    yaw: self.aim.yaw,
                    pitch: self.aim.pitch,
                };
                let kills = self.net.board().and_then(|b| b.rows.iter().find(|r| r.id as u16 == id).map(|r| r.kills as u32)).unwrap_or(0);
                (Some(p.build_ghost(id, self.last_input_tick, self.build_sel, kills, &aim)), p.blocks.pieces())
            }
            _ => (None, self.net.block_pieces()),
        };
        let lock = self.pred.as_ref().and_then(|p| {
            let l = &p.combat.lock;
            (l.phase != LockPhase::Search).then(|| LockOn {
                target: l.target_id as EntityId,
                pos: l.pos_now(),
                progress: l.progress,
                locked: l.phase == LockPhase::Locked,
                phase: l.phase,
                eased: l.eased(),
                t_since_phase: l.t_since_phase,
                target_vel: [l.target_vel.x, l.target_vel.y, l.target_vel.z],
                blend: l.blend,
            })
        });
        // yaw / pitch stay the live aim; the rest of the pose is blended from the previous tick's to this one's
        let a = ((local_tick - (self.pose_t - 1.0)) as f32).clamp(0.0, 1.0);
        let (mut camera, mut weapon) = self.pose;
        if self.pose_t.is_finite() {
            let (pc, pw) = self.pose_prev;
            let l = |x: f32, y: f32| x + (y - x) * a;
            camera.roll = l(pc.roll, camera.roll);
            camera.y_offset = l(pc.y_offset, camera.y_offset);
            camera.offset = [l(pc.offset[0], camera.offset[0]), l(pc.offset[1], camera.offset[1])];
            camera.fov = l(pc.fov, camera.fov);
            weapon = WeaponPose { pitch: l(pw.pitch, weapon.pitch), back: l(pw.back, weapon.back), y_dip: l(pw.y_dip, weapon.y_dip) };
        }
        let eye = local_pos.map_or([0.0; 3], |p| [p[0], p[1] + self.cfg.sim.eye_above_center + camera.y_offset, p[2]]);
        // line of sight to each remote's chest, against level geometry only (not player capsules, not sensors)
        if local_tick - self.los_cache.0 >= 6.0 || local_tick < self.los_cache.0 {
            let ids = match &self.pred {
                Some(p) => remotes
                    .iter()
                    .filter(|(_, smp)| {
                        let chest = [smp.pos[0], smp.pos[1] + 0.9, smp.pos[2]];
                        let d = [chest[0] - eye[0], chest[1] - eye[1], chest[2] - eye[2]];
                        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                        if len < 0.3 {
                            return true;
                        }
                        let ray = Ray::new(Point::new(eye[0], eye[1], eye[2]), vector![d[0] / len, d[1] / len, d[2] / len]);
                        let f = QueryFilter::default()
                            .exclude_sensors()
                            .groups(InteractionGroups::new(GROUP_MELEE, shinobi_sim::combat::solid_groups()));
                        p.world.queries.cast_ray(&p.world.bodies, &p.world.colliders, &ray, len - 0.3, true, f).is_none()
                    })
                    .map(|(id, _)| *id)
                    .collect(),
                None => remotes.iter().map(|(id, _)| *id).collect(),
            };
            self.los_cache = (local_tick, ids);
        }
        let los_ids = self.los_cache.1.clone();
        let events = self.net.take_events();
        let power_world = match (self.net.render_tick(local_tick), self.net.interp.newest_tick().and_then(|t| self.net.interp.get(t))) {
            (Some(rt), Some(newest)) => PowerWorld::of(newest, |id| self.net.interp.sample(id, rt).map(|s| s.pos)),
            (None, Some(newest)) => PowerWorld::of(newest, |_| None),
            _ => PowerWorld::default(),
        };
        let (own_live, own_armed) = power_world.tags_of(own_id.unwrap_or(0));
        let live_for_view = own_live.max(self.tag_throws.len() as u32);
        let power = PowerView::of(&self.power, &PowerCfg::default(), live_for_view, own_armed).filter(|_| self.death.is_none());
        // who killed us (for the kill-cam): the Kill event can land before or after the dead flag
        if let Some(me) = own_id {
            for e in &events {
                if let NetEvent::Kill { killer, victim, .. } = e {
                    if *victim == me && *killer != 0 {
                        match &mut self.death {
                            Some(d) if d.killer.is_none() => d.killer = Some(*killer),
                            Some(_) => {}
                            None => self.last_kill = Some((*killer, local_tick)),
                        }
                    }
                }
            }
        }
        let (mut aim, mut eye, mut camera, mut weapon) = (self.aim, eye, camera, weapon);
        let death = self.death.as_mut().map(|d| {
            let t = ((local_tick - d.t0) / shinobi_proto::TICK_HZ as f64).max(0.0) as f32;
            let killer_pos = d.killer.and_then(|k| remotes.iter().find(|(id, _)| *id == k).map(|(_, s)| s.pos));
            let side = if d.killer.unwrap_or(0) % 2 == 0 { 1.0 } else { -1.0 };
            let pose = crate::death_cam::death_pose(t, d.pos, d.yaw0, d.pitch0, killer_pos, self.cfg.sim.eye_above_center, side);
            aim.yaw = pose.yaw;
            aim.pitch = pose.pitch;
            eye = pose.eye;
            camera.roll = pose.roll;
            camera.y_offset = 0.0;
            camera.offset = pose.shake;
            camera.fov = if camera.fov > 1.0 { camera.fov } else { 90.0 } + pose.fov_add;
            weapon = WeaponPose { pitch: weapon.pitch, back: weapon.back, y_dip: 1.2 }; // the hands drop out of frame
            let fresh = std::mem::take(&mut d.fresh);
            DeathView { t, total: self.net.respawn_secs().unwrap_or(2.5), pos: d.pos, killer: d.killer, fresh }
        });
        let dead = death.is_some();
        RenderView {
            blocking: self.blocking,
            star: self.star_est,
            star_sel: self.star_sel,
            star_cycled: self.star_cycled,
            star_throws: self.star_throws,
            star_objs: Self::star_objects_of(&self.net, self.pred.as_ref(), own_id),
            disc_kinds,
            blind: self.blind_frac().map_or(0.0, |f| self.blind_strength * whiteout_level(f)),
            dazed: self.blind_frac().unwrap_or(0.0),
            hidden_ids: self.hidden_ids.clone(),
            charging: self.fire_hold.map_or(0.0, |h| (h as f32 / MARK_CHARGE_TICKS as f32).min(1.0)),
            star_refused: self.star_refused,
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
            los_ids,
            remotes,
            events,
            death,
            build_ghost: build_ghost.filter(|_| !dead),
            lock: lock.filter(|_| !dead),
            lock_edges: std::mem::take(&mut self.lock_edges),
            power,
            power_world,
            power_events: std::mem::take(&mut self.power_events),
            blocks,
            block_size: BlockCfg::default().size,
            block_thickness: BlockCfg::default().thickness,
            aim,
            discs,
            disc_owners,
            rope: self.pred.as_ref().and_then(|p| p.ch.grapple).filter(|_| !dead).map(|g| (eye, [g.anchor.x, g.anchor.y, g.anchor.z])),
            controls: self.controls.rows(),
            district: local_pos.map(|p| self.level.district_at(p[0], p[2])),
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

#[cfg(test)]
mod whiteout_tests {
    use super::whiteout_level;

    #[test]
    fn whiteout_is_solid_at_first_then_fades_to_nothing_with_the_daze() {
        assert_eq!(whiteout_level(1.0), 1.0);
        assert_eq!(whiteout_level(0.75), 1.0, "solid for the first quarter");
        assert_eq!(whiteout_level(0.0), 0.0, "clear exactly when the daze ends");
        let mut last = 1.0;
        for i in 0..=100 {
            let v = whiteout_level(1.0 - i as f32 / 100.0);
            assert!(v <= last + 1e-6, "never brightens again: {v} after {last}");
            assert!((0.0..=1.0).contains(&v));
            last = v;
        }
        assert!(whiteout_level(0.4) > 0.3 && whiteout_level(0.4) < 0.9, "mid-fade is visible but see-through");
        assert_eq!(whiteout_level(2.0), 1.0);
        assert_eq!(whiteout_level(-1.0), 0.0);
    }
}
