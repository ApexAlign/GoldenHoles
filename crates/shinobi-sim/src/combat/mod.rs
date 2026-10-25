//! Weapons, melee and pickups. Engine-agnostic (raw rapier3d), deterministic, no ECS.
//!
//! Sim side (server + client prediction): `CombatSystem::update` -> `CombatEvent`s.
//! Client-only cosmetics: `viewmodel::KatanaViewModel` consumes the events.
//!
//! Per tick, BEFORE `CharacterSystem::update`:
//! ```ignore
//! let aim = Aim::level(eye_pos, dq_yaw(intent.aim_yaw));
//! combat.update(dt, now, &CombatInput::new(intent.buttons.has(Buttons::FIRE), intent.buttons.has(Buttons::SLICE)), &aim, &world, &ch, &mut player_input, &mut events);
//! ch.update(dt, &player_input, &mut world, now);
//! ```
//! Collision groups reuse `character.rs` numbering (ENEMY = GROUP_2, TRAIL = GROUP_3).
//! Every non-enemy collider (level geometry, player) needs a membership that EXCLUDES `GROUP_ENEMY`
//! (`GROUP_WORLD` / `GROUP_PLAYER`): the default `Group::ALL` membership reads as "enemy".
//! Enemy colliders must: be in `GROUP_ENEMY`, accept `GROUP_PROJECTILE | GROUP_MELEE`, and carry
//! their id in `Collider::user_data`.
//! Call `world.queries.update(&world.colliders)` once per tick after enemies move.

use rapier3d::prelude::*;

use crate::character::{CharacterSystem, PhysicsWorld, PlayerInput, GROUP_ENEMY, GROUP_TRAIL};

pub mod aim_assist;
pub mod flow;
pub mod katana;
pub mod powerup;
pub mod shuriken;
pub mod stars;
pub mod weapon;

pub const GROUP_WORLD: Group = Group::GROUP_1;
pub const GROUP_PLAYER: Group = Group::GROUP_7;
/// Parentless sensor capsule per player (`user_data` = player id). Shots/slashes target it; see `pvp.rs`.
pub const GROUP_HITBOX: Group = Group::GROUP_8;
/// What shurikens, slashes and the ultimate can damage.
pub const TARGETS: Group = GROUP_ENEMY.union(GROUP_HITBOX);
/// Solid movement capsule of an NPC enemy (the shootable part is its sensor hitbox in `GROUP_ENEMY`).
pub const GROUP_ENEMY_BODY: Group = Group::GROUP_6;
pub const GROUP_PROJECTILE: Group = Group::GROUP_4;
pub const GROUP_MELEE: Group = Group::GROUP_5;

/// The live BOUNTY's own lock-on range multiplier (`CombatSystem::assist_boost`, docs/bounty.md): the
/// counter-play to a committed hunter's wider hook reach (`fsm::hook_decision` in shinobi-ai).
pub const BOUNTY_ASSIST_RANGE_MULT: f32 = 1.5;

/// Everything except targets, player bodies and (sensor) light-walls: what blocks shots and slashes.
pub fn solid_groups() -> Group {
    Group::ALL.difference(TARGETS).difference(GROUP_TRAIL).difference(GROUP_PLAYER).difference(GROUP_ENEMY_BODY)
}

/// Groups a projectile may touch: solids + targets (not trails, not player movement capsules).
pub fn projectile_groups() -> Group {
    Group::ALL.difference(GROUP_TRAIL).difference(GROUP_PLAYER).difference(GROUP_ENEMY_BODY)
}

/// Sensors are skipped (light-walls etc.) except player and enemy hitboxes, which are sensors by design.
pub fn passes_projectile(_: ColliderHandle, c: &Collider) -> bool {
    !c.is_sensor() || c.collision_groups().memberships.intersects(GROUP_HITBOX.union(GROUP_ENEMY))
}

pub fn is_target(w: &PhysicsWorld, h: ColliderHandle) -> bool {
    w.colliders.get(h).map_or(false, |c| c.collision_groups().memberships.intersects(TARGETS))
}

/// Yaw 0 faces -Z (same as `CharacterSystem::facing`), positive yaw turns toward -X (CCW about +Y).
/// Network intents carry yaw only, so `pitch` is 0 for intent-driven aim: slashes are horizontal and
/// shurikens fly level. Clients with pitch can set it, but then the server must receive it too.
#[derive(Clone, Copy, Debug)]
pub struct Aim {
    pub origin: Vector<Real>,
    pub yaw: f32,
    pub pitch: f32,
}

impl Aim {
    /// From a network intent: `Aim::level(eye, net::quant::dq_yaw(intent.aim_yaw))`.
    pub fn level(origin: Vector<Real>, yaw: f32) -> Self {
        Self { origin, yaw, pitch: 0.0 }
    }

    pub fn forward(&self) -> Vector<Real> {
        let (sy, cy) = (libm::sinf(self.yaw), libm::cosf(self.yaw));
        let (sp, cp) = (libm::sinf(self.pitch), libm::cosf(self.pitch));
        Vector::new(-sy * cp, sp, -cy * cp)
    }

    pub fn flat_forward(&self) -> Vector<Real> {
        let (sy, cy) = (libm::sinf(self.yaw), libm::cosf(self.yaw));
        Vector::new(-sy, 0.0, -cy)
    }
}

/// One-tick button edges relevant to combat.
#[derive(Clone, Copy, Default, Debug)]
pub struct CombatInput {
    pub fire: bool,
    pub slice: bool,
    /// No wire bit yet (Buttons uses all 8 bits); the game maps a key here.
    pub ultimate: bool,
    /// With `fire`: a charged throw, the MARK star (see `ShurikenCfg::mark_cost`).
    pub mark: bool,
    /// With `fire` (and no `mark`): which throwable leaves the hand (`stars::StarKind`).
    pub star: stars::StarKind,
}

impl CombatInput {
    /// From a network intent: `CombatInput::new(b.has(Buttons::FIRE), b.has(Buttons::SLICE))`.
    pub fn new(fire: bool, slice: bool) -> Self {
        Self { fire, slice, ultimate: false, mark: false, star: stars::StarKind::Standard }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageKind {
    Shuriken,
    Slash,
    GridBeam,
    /// A yumi arrow (`power.rs`).
    Arrow,
    /// An explosive fuda tag's blast (`power.rs`).
    Tag,
}

/// Which katana move a `CombatEvent::Slash` was (docs/melee.md, "Katana moves").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SlashKind {
    /// The plain cut / combo / hook-strike / wall slash.
    #[default]
    Light,
    /// Charged cut (hold after a cut, release): more damage and reach, slower recovery.
    Heavy,
    /// Cut while airborne: a downward dive-slash that slams the victim.
    Air,
    /// 360 degree cut (slide, or just after lowering the shield): everyone close.
    Whirl,
    /// Cut right after a parry: finds the staggered attacker by itself.
    Counter,
}

/// A named katana combo a connecting swing completed (docs/melee.md "Combos"): the HUD callout and the announcer key
/// off it. `None` for an ordinary cut (and for every swing that missed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KatanaCombo {
    #[default]
    None,
    /// The third connected cut of a combo.
    Finisher,
    /// The wall-run chain's closing kick.
    WallKick,
    /// Heavy into heavy.
    TwinFang,
    /// Cut, cut, heavy.
    Breaker,
    /// Heavy (the launch) into an air cut.
    RisingStrike,
    /// Dash into a cut.
    DashStrike,
}

impl KatanaCombo {
    /// HUD callout text; `None` for no combo.
    pub fn label(self) -> Option<&'static str> {
        match self {
            KatanaCombo::None => None,
            KatanaCombo::Finisher => Some("FINISHER"),
            KatanaCombo::WallKick => Some("WALL KICK"),
            KatanaCombo::TwinFang => Some("TWIN FANG"),
            KatanaCombo::Breaker => Some("BREAKER"),
            KatanaCombo::RisingStrike => Some("RISING STRIKE"),
            KatanaCombo::DashStrike => Some("DASH STRIKE"),
        }
    }
}

/// Server-side facts the katana moves need but the weapon cannot see (the shield state lives on the server). The server
/// fills it before every `CombatSystem::update`; the client leaves it empty (these moves only change who is hit and how
/// hard, which the server decides).
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveCtx {
    /// A parry just landed: the staggered attacker's hitbox and the damage that was negated.
    pub counter: Option<(ColliderHandle, f32)>,
    /// The shield came down this very moment (a press now is a whirl cut).
    pub just_unblocked: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum CombatEvent {
    /// `target` = enemy collider `user_data`.
    Damage { target: u128, collider: ColliderHandle, amount: f32, kind: DamageKind, point: Point<Real> },
    /// Katana swing. `lunge`: the dash actually fired (false = on cooldown, slash only).
    Slash {
        flip: bool,
        dir: Vector<Real>,
        lunge: bool,
        hits: u32,
        /// Slash volume (for lag-comp sweeps): starts at `origin`, `reach` long after wall truncation.
        origin: Point<Real>,
        reach: f32,
        half_width: f32,
        half_height: f32,
        damage: f32,
        kind: SlashKind,
        /// The named combo this swing completed (`KatanaCombo::None` for a plain cut or a miss).
        combo: KatanaCombo,
    },
    ShurikenThrown { pos: Point<Real>, dir: Vector<Real>, kind: stars::StarKind },
    /// A flash star burst at `pos` (impact or fuse). The server turns it into white-outs (`StarBlind`).
    StarFlash { pos: Point<Real> },
    /// Server only: `victim` is white-out blind for `ticks` at `strength_q` / 255 (events of the thrower).
    StarBlind { victim: u16, strength_q: u8, ticks: u16 },
    /// A power weapon moment for sound / FX (`power::FX_*`), at `pos` (server only, forwarded as `NetEvent::PowerFx`).
    PowerFx { what: u8, pos: Point<Real> },
    /// A MARK star struck `target` (`collider` = its hitbox): no damage, the server marks that player for the katana.
    MarkHit { target: u128, collider: ColliderHandle },
    ShurikenBounce { pos: Point<Real>, normal: Vector<Real> },
    ShurikenGone { pos: Point<Real> },
    /// Pushed just before the `Damage` of a disc that had already bounced when it hit `collider` (ricochet).
    RicochetHit { collider: ColliderHandle },
    /// Feed to `FpsCamera.recoil.kick`.
    Recoil { pitch_deg: f32, yaw_deg: f32 },
    PickedUp { id: u32, kind: powerup::PowerupKind },
    UltimateStart { origin: Point<Real> },
    /// Wrist grapple hooked: rope runs from the eye `origin` to the static `anchor` (docs/grapple.md).
    GrappleAttach { origin: Point<Real>, anchor: Point<Real> },
    /// Grapple fired at nothing hookable (sky, out of range, a player): play the 'fizzle' feedback.
    GrappleMiss { origin: Point<Real>, dir: Vector<Real> },
    /// Rope released (zip again, jump, arrived, timeout, line of sight lost).
    GrappleRelease { pos: Point<Real> },
    /// The katana connected with `target` (player id / NPC id) at `point`. `killed` is filled in by the server once
    /// the damage has been applied (always false in predicted, client-side events).
    SlashHit { target: u128, point: Point<Real>, killed: bool },
    /// The katana swing found nothing: play the whiff (dim arc, small recover).
    SlashMiss { origin: Point<Real>, dir: Vector<Real> },
    /// A weapon struck the player-built piece in `cell` for `amount`. The server turns it into `Blocks::damage`
    /// (which may de-rez the piece); the client predictor ignores it.
    PieceHit { cell: [i32; 3], amount: f32, kind: DamageKind },
}

pub struct CombatSystem {
    pub loadout: weapon::Loadout,
    /// Teammates' hitbox colliders, refreshed by the server every tick (empty = free-for-all).
    pub allies: Vec<ColliderHandle>,
    pub powerups: powerup::PowerupState,
    /// Predator lock-on (docs/lock-on-and-hud.md): same state machine on server and client predictor.
    pub lock: aim_assist::LockState,
    pub lock_cfg: aim_assist::LockCfg,
    /// Cone (degrees) and range a target must be inside to be acquired; from `combat.shuriken.assist_*`.
    pub assist_cone_deg: f32,
    pub assist_range: f32,
    /// Edge of the lock state machine during the most recent `update` (cleared at the start of the next one).
    pub lock_edge: Option<aim_assist::LockEdge>,
    /// Blinded (flash star): no lock-on can form and the lock that was held breaks; set every tick by the owner of the sim.
    pub suppress_lock: bool,
    /// This player is the live BOUNTY (docs/bounty.md) right now: set every tick by the owner of the sim, the
    /// same way `suppress_lock` is. The bounty's own lock-on reaches farther (`BOUNTY_ASSIST_RANGE_MULT`) --
    /// the counter-play to a committed hunter's own wider hook reach (`fsm::hook_decision`): a wanted player can
    /// pick a pursuer off at range before their traversal closes the gap, instead of only being chased.
    pub assist_boost: bool,
    /// Hitboxes never acquired as a lock-on target (they still take hits); unused, kept for hiding effects.
    pub hidden: Vec<ColliderHandle>,
    /// Flow streak: chained traversal moves (wall-run, hook, dash, wall-jump) without a ground reset
    /// (docs/traversal_melee.md, "Flow"). Feeds a move-speed multiplier and a katana damage bonus.
    pub flow: flow::Flow,
}

impl CombatSystem {
    /// Own in-flight projectiles (position, direction) for the renderer.
    pub fn projectiles(&self) -> Vec<([f32; 3], [f32; 3])> {
        let mut v = Vec::new();
        self.loadout.projectiles(&mut v);
        v
    }

    pub fn new(loadout: weapon::Loadout) -> Self {
        let s = shuriken::ShurikenCfg::default();
        Self {
            loadout,
            allies: Vec::new(),
            powerups: powerup::PowerupState::default(),
            lock: aim_assist::LockState::default(),
            lock_cfg: aim_assist::LockCfg::default(),
            assist_cone_deg: s.assist_cone_deg,
            assist_range: s.assist_range,
            lock_edge: None,
            suppress_lock: false,
            assist_boost: false,
            hidden: Vec::new(),
            flow: flow::Flow::new(flow::FlowCfg::default()),
        }
    }

    /// LMB/fire = neon shurikens, slice = cyber-katana.
    pub fn ninja(ch: &crate::character::Cfg) -> Self {
        Self::new(
            weapon::Loadout::default()
                .with(shuriken::ShurikenLauncher::default())
                .with(katana::Katana::new(katana::KatanaCfg::for_character(ch))),
        )
    }

    /// Load tunables from a `Balance` (weapons + ultimate).
    pub fn apply_balance(&mut self, b: &crate::balance::Balance, ch: &crate::character::Cfg) {
        self.loadout.apply_balance(b, ch);
        self.lock_cfg = aim_assist::LockCfg { lock_secs: b.combat.lock_secs, pull_min: b.combat.lock_pull_min, break_secs: b.combat.lock_break_secs, blend_secs: b.combat.lock_blend_secs };
        self.assist_cone_deg = b.combat.shuriken.assist_cone_deg;
        self.assist_range = b.combat.shuriken.assist_range;
        self.powerups.ultimate = crate::combat::powerup::UltimateCfg {
            speed: b.combat.ultimate_speed,
            max_radius: b.combat.ultimate_max_radius,
            damage: b.combat.ultimate_damage,
        };
        self.flow.apply_balance(b.flow_cfg());
    }

    /// Server-side move context for this tick (see `MoveCtx`); call before `update`.
    pub fn set_moves(&mut self, c: MoveCtx) {
        self.loadout.moves = c;
    }

    /// Heavy-cut charge, 0 (idle) .. 1 (full): drives the charge ring on the HUD.
    pub fn slice_charge(&self) -> f32 {
        self.loadout.charge(weapon::Trigger::Secondary)
    }

    /// Skill combo counter (connected katana cuts in a row) for the HUD.
    pub fn combo_count(&self) -> u32 {
        self.loadout.combo_count()
    }

    /// The owner took damage: the combo counter, wall chain and flow streak reset.
    pub fn on_hurt(&mut self) {
        self.loadout.on_hurt();
        self.flow.on_hurt();
    }

    /// Flow streak (chained traversal moves) for the HUD.
    pub fn flow_streak(&self) -> u32 {
        self.flow.streak()
    }

    /// Flow tier (0..3) for the HUD / VFX.
    pub fn flow_tier(&self) -> u32 {
        self.flow.tier()
    }

    /// Star energy as (charges, pool size, locked out); `None` when the balance has no pool.
    pub fn shuriken_energy(&self) -> Option<(f32, f32, bool)> {
        self.loadout.energy()
    }

    /// Katana (secondary trigger) cooldown left, seconds. Mirrored into `CharacterSystem::slice_cooldown`
    /// after every combat tick so it travels in `OwnState` and the client can predict the lunge.
    pub fn slice_cooldown_left(&self) -> f32 {
        self.loadout.cooldown_left(weapon::Trigger::Secondary)
    }

    pub fn set_slice_cooldown_left(&mut self, v: f32) {
        self.loadout.set_cooldown_left(weapon::Trigger::Secondary, v);
    }

    /// Katana press queued while the blade recovers (seconds left, `katana::PRESS_BUFFER_S` at most). Mirrored into
    /// `CharacterSystem::slice_buffer` next to the cooldown, for the same reason.
    pub fn slice_buffer_left(&self) -> f32 {
        self.loadout.buffered(weapon::Trigger::Secondary)
    }

    pub fn set_slice_buffer_left(&mut self, v: f32) {
        self.loadout.set_buffered(weapon::Trigger::Secondary, v);
    }

    /// True while this player's combat will query the world this tick regardless of input (discs / bursts
    /// in flight). Lets the server skip lag-comp work for idle players.
    pub fn busy(&self) -> bool {
        self.loadout.active() || !self.powerups.bursts().is_empty()
    }

    /// Owner died: drop everything in flight.
    pub fn reset_projectiles(&mut self) {
        self.loadout.reset();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        dt: f32,
        now: f32,
        input: &CombatInput,
        aim: &Aim,
        world: &PhysicsWorld,
        ch: &CharacterSystem,
        player_input: &mut PlayerInput,
        events: &mut Vec<CombatEvent>,
    ) {
        // lock first: one best_target per tick feeds the lock state and every weapon fired this tick
        let (eye, fwd) = (aim.origin, aim.forward());
        let held = self.lock.target.filter(|_| self.lock.holding());
        // hidden players are skipped by the lock like allies
        let mut skip = self.allies.clone();
        skip.extend(self.hidden.iter().copied());
        let range = if self.assist_boost { self.assist_range * BOUNTY_ASSIST_RANGE_MULT } else { self.assist_range };
        let found = if self.suppress_lock { None } else { aim_assist::best_target_held(world, eye, fwd, self.assist_cone_deg, range, &skip, ch.body, held) };
        self.lock_edge = self.lock.update(dt, &self.lock_cfg, found.as_ref());
        let hint = found.filter(|_| self.lock.holding()).map(|t| aim_assist::LockHint { dir: t.dir, pull: self.lock.pull(&self.lock_cfg), collider: t.collider, centre: t.centre, vel: self.lock.target_vel });
        // cancel: a dash, hook or wall jump started out of a cut frees the blade at once (costs a little star energy)
        let hook = player_input.grapple_aim.is_some() && ch.grapple.is_none() && ch.grapple_cooldown() <= 0.0;
        if (player_input.dash_pressed && ch.dash_ready()) || hook || (player_input.jump_pressed && ch.wall_normal().is_some()) {
            self.loadout.cancel_slice();
        }
        // Flow (docs/traversal_melee.md "Flow"): a streak of chained traversal moves. `ch` is one tick old here
        // (this runs before `CharacterSystem::update`), same lag the hook / wall-jump air-cut bonuses already live
        // with, so the edges below read as "just happened" for exactly one tick.
        let grounded = ch.state == crate::character::MoveState::Grounded;
        let flat_speed = (ch.vel.x * ch.vel.x + ch.vel.z * ch.vel.z).sqrt();
        self.flow.ground_check(grounded, flat_speed);
        let moved = ch.dash_age <= 1e-6
            || ch.wallrun_age <= 1e-6
            || ch.walljump_age <= 1e-6
            || matches!(ch.grapple_event, crate::character::GrappleEvent::Attached { .. });
        if moved {
            self.flow.add_move();
        }
        player_input.speed_boost = (self.flow.speed_mult() - 1.0).max(0.0);
        self.loadout.update(dt, now, input, aim, world, ch, player_input, events, &self.allies, hint, self.flow.dmg_mult());
        self.powerups.update(dt, input, aim, world, ch.body, events);
    }
}

#[cfg(test)]
mod tests;
