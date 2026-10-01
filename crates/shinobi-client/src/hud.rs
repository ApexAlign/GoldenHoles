//! Cybernetic helmet-visor HUD (renderer-agnostic).
//! Pure math: `Hud::build` turns gameplay state into a flat list of draw primitives in
//! normalized screen space (0,0 = top-left, 1,1 = bottom-right) that any backend can draw.
//!
//! Design rule: peripheral vision stays clear so fast projectiles stay trackable.
//! Content lives only in the "brow" (top) and "chin" (bottom) bands around a small center
//! reticle. The left/right `SIDE_CLEAR` columns hold nothing but hairline rails, and the
//! vignette is capped and zero inside `VIGNETTE_START`.

use shinobi_proto::snapshot::{FLAG_DEAD, FLAG_INVULN, MoveKind};
use shinobi_sim::combat::{CombatEvent, DamageKind};
use shinobi_proto::gameplay::{KIND_GRID_BEAM, KIND_SHURIKEN, KIND_SLASH, PHASE_INTERMISSION, PHASE_LIVE};
use shinobi_sim::events::NetEvent;
use shinobi_sim::match_state::{EndReason, MatchEvent, MatchState, PhaseKind, ScoreEntry};
use shinobi_sim::pvp::{PvpEvent, Vitals};

use crate::netclient::NetClient;

pub const SIDE_CLEAR: f32 = 0.22; // columns [0,SIDE_CLEAR] and [1-SIDE_CLEAR,1] stay content-free
pub const BROW_H: f32 = 0.07;
pub const CHIN_TOP: f32 = 0.86;
pub const VIGNETTE_START: f32 = 0.78; // radial distance (corner = 1) where the shade begins
pub const VIGNETTE_MAX_A: f32 = 0.22;
/// Peak edge flash when hit (transient, fades over `DAMAGE_TIME`); the only vignette allowed above the cap.
pub const DAMAGE_FLASH_A: f32 = 0.5;
pub const RAIL_W: f32 = 0.0025; // hairline visor rail thickness

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub const CYAN: Rgba = Rgba(0.0, 0.94, 1.0, 1.0);
    pub const MAGENTA: Rgba = Rgba(1.0, 0.1, 0.65, 1.0);
    pub const AMBER: Rgba = Rgba(1.0, 0.72, 0.1, 1.0);
    pub const GOLD: Rgba = Rgba(1.0, 0.84, 0.3, 1.0);
    pub fn a(self, a: f32) -> Self {
        Rgba(self.0, self.1, self.2, a)
    }
}

/// Single source of truth for UI colour (HUD, touch overlay, and Graphics' menus/3D accents).
/// 1980s Tron: cyan = friendly/ready, orange = cooldown/katana/warning, magenta = danger/damage,
/// over a near-black blue grid. Everything in `hud` and `touch_ui` reads from here, never a literal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub cyan: Rgba,
    pub orange: Rgba,
    pub magenta: Rgba,
    pub gold: Rgba,
    pub bg: Rgba,
    pub grid: Rgba,
    pub text: Rgba,
}

impl Palette {
    pub const TRON: Palette = Palette {
        cyan: Rgba(0.0, 0.94, 1.0, 1.0),
        orange: Rgba(1.0, 0.55, 0.05, 1.0),
        magenta: Rgba(1.0, 0.1, 0.65, 1.0),
        gold: Rgba(1.0, 0.84, 0.3, 1.0),
        bg: Rgba(0.02, 0.03, 0.08, 1.0),
        grid: Rgba(0.05, 0.35, 0.45, 1.0),
        text: Rgba(0.82, 0.98, 1.0, 1.0),
    };

    /// Colour for a damage source (hit marker, killfeed weapon tag).
    /// Team colour: 0 = cyan, anything else = orange.
    pub fn team(&self, t: u8) -> Rgba {
        if t == 0 { self.cyan } else { self.orange }
    }

    pub fn for_damage(&self, k: DamageKind) -> Rgba {
        match k {
            DamageKind::Shuriken => self.cyan,
            DamageKind::Slash => self.orange,
            DamageKind::GridBeam => self.magenta,
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::TRON
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// Filled rect; `glow` = bloom radius in screen fraction (0 = none).
    Rect { r: Rect, color: Rgba, glow: f32 },
    Line { a: (f32, f32), b: (f32, f32), color: Rgba, glow: f32 },
    /// Screen-edge radial shade; shader evaluates `vignette_alpha`.
    Vignette { start: f32, max_a: f32, tint: Rgba },
    /// Faint CRT scanlines: `count` lines, `scroll` in [0,1), alpha `a`.
    Scanlines { count: u32, scroll: f32, a: f32 },
    /// Circle: `r` in fractions of screen HEIGHT (backend corrects for aspect); `fill` = solid disc.
    Ring { c: (f32, f32), r: f32, color: Rgba, fill: bool, glow: f32 },
    /// Text anchored at `pos` (baseline-left/center/right per `align`); `size` = glyph height, screen fraction.
    Text { pos: (f32, f32), text: String, size: f32, color: Rgba, align: Align },
    /// Tron floor grid clipped to `r`; `cell` in screen-height fractions.
    Grid { r: Rect, cell: f32, color: Rgba },
}

#[derive(Clone, Debug, PartialEq)]
pub struct KillEntry {
    pub killer: String,
    pub victim: String,
    pub weapon: DamageKind,
    /// Seconds since the kill; entries older than `KILLFEED_TIME` are not drawn.
    pub age: f32,
    /// Local player is killer or victim (brighter row).
    pub involves_local: bool,
    /// Environmental kill (obstacle / fall / no attacker): drawn with the void glyph, no killer name.
    pub void: bool,
    /// Team ids (0 cyan, 1 orange) when the match is team-based: colours the names.
    pub killer_team: Option<u8>,
    pub victim_team: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoreRow {
    pub name: String,
    pub kills: u16,
    pub deaths: u16,
    pub ping_ms: u16,
    pub local: bool,
    pub alive: bool,
    /// 0 cyan, 1 orange; `None` = free-for-all (flat list).
    pub team: Option<u8>,
}

/// Team match score for the top bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeamScores {
    /// The local player's team.
    pub mine: u8,
    pub scores: [u16; 2],
    /// Kills that win the match.
    pub limit: u16,
}

/// A friendly name tag, already projected: normalized screen position (clamped into the central band when the
/// ally is off-screen), metres away, and whether it is clamped. Allies only: enemies never get one.
#[derive(Clone, Debug, PartialEq)]
pub struct AllyTag {
    pub pos: (f32, f32),
    pub name: String,
    pub dist: f32,
    pub clamped: bool,
}

/// Plain data the HUD draws from. Fill it once per frame from the latest snapshot / server events plus
/// the local `HudFx`; `Hud` never reaches into sim or net state itself.
#[derive(Clone, Debug, Default)]
pub struct HudState {
    pub hp: f32,            // 0..=1
    pub dash_ready: f32,    // 0..=1 (1 = ready); katana lunge shares the dash cooldown
    pub shuriken_ready: f32, // 0..=1
    pub slice_ready: f32,    // 0..=1 katana
    /// Crosshair spread 0..=1 (moving / airborne / just threw): widens the reticle gap.
    pub spread: f32,
    pub ult: f32,           // 0..=1 (1 = ready)
    pub wall_run: bool,
    pub invulnerable: bool,
    /// Seconds since last enemy hit landed, and what landed it (None = never / expired).
    pub since_hit: Option<f32>,
    pub hit_kind: Option<DamageKind>,
    /// The last dealt hit killed its target (bigger marker).
    pub hit_kill: bool,
    /// Damage dealt by the last hit, whole points (0 = unknown): shown as a rising number.
    pub hit_amount: u8,
    /// Seconds since damage taken and its bearing: radians, 0 = ahead, +ve = clockwise.
    pub since_damage: Option<(f32, f32)>,
    /// Size of the last hit taken as a fraction of max HP (0 = unknown): widens the damage wedge.
    pub damage_frac: f32,
    /// Shuriken ammo `(loaded, max)` once the weapon has a magazine; `None` hides the readout.
    pub shuriken_ammo: Option<(u8, u8)>,
    /// Block-building slot: `(blocks left, cooldown ready 0..=1)`; `None` until building exists.
    pub block_slot: Option<(u8, f32)>,
    pub team_scores: Option<TeamScores>,
    pub ally_tags: Vec<AllyTag>,
    pub ping_ms: Option<u16>,
    pub killfeed: Vec<KillEntry>,
    /// Hold-to-show (Tab / touch button).
    pub show_scoreboard: bool,
    pub scoreboard: Vec<ScoreRow>,
    pub dead: bool,
    /// Match phase + whole seconds left (None = warmup waiting for players).
    pub match_clock: Option<(PhaseKind, Option<u32>)>,
    /// 0-based map index shown in the scoreboard header.
    pub match_map: Option<u32>,
    pub banner: Option<Banner>,
    /// Short pickup message above the chin ("+ GRID BEAM"), age in seconds.
    pub toast: Option<(String, f32)>,
    /// Seconds until respawn while dead.
    pub respawn_in_s: Option<f32>,
    /// Undecodable datagrams arrived since the caller's baseline; ping readout turns magenta with "!".
    pub net_warn: bool,
    pub time: f32,
}

/// Data the snapshot stream does not carry yet (hp, cooldowns, kill/score bookkeeping, RTT). The game loop
/// fills it from local prediction / server events; `HudState::from_frame` merges it with snapshot truth.
#[derive(Clone, Debug, Default)]
pub struct LocalStats {
    pub hp: f32,
    pub dash_ready: f32,
    pub shuriken_ready: f32,
    pub slice_ready: f32,
    pub spread: f32,
    pub ult: f32,
    pub ping_ms: Option<u16>,
    pub killfeed: Vec<KillEntry>,
    pub show_scoreboard: bool,
    pub scoreboard: Vec<ScoreRow>,
    /// `NetClient::bad_datagrams` at the last time the caller acknowledged it (e.g. once per second).
    pub bad_datagrams_baseline: u32,
    pub respawn_in_s: Option<f32>,
    pub match_clock: Option<(PhaseKind, Option<u32>)>,
    pub match_map: Option<u32>,
    /// Spawn protection active: gold chrome like i-frames.
    pub spawn_protected: bool,
}

impl HudState {
    /// Build this frame's HUD data. From the network layer: connection state, the local entity's
    /// dead / invulnerable / wall-run status out of the newest snapshot, and the bad-datagram warning.
    /// Everything else comes from `stats` and the transient `fx`. Before `Welcome` or before the first
    /// snapshot containing the local player, snapshot-derived flags are simply false.
    pub fn from_frame(net: &NetClient, stats: &LocalStats, fx: &HudFx, time: f32) -> HudState {
        let me = net
            .player_id()
            .zip(net.interp.newest_tick())
            .and_then(|(id, t)| net.interp.get(t)?.get(id).copied());
        let dead = me.map_or(false, |e| e.flags & FLAG_DEAD != 0);
        let mut s = HudState {
            hp: if dead { 0.0 } else { stats.hp.clamp(0.0, 1.0) },
            dash_ready: stats.dash_ready.clamp(0.0, 1.0),
            shuriken_ready: stats.shuriken_ready.clamp(0.0, 1.0),
            slice_ready: stats.slice_ready.clamp(0.0, 1.0),
            spread: stats.spread.clamp(0.0, 1.0),
            ult: stats.ult.clamp(0.0, 1.0),
            wall_run: me.map_or(false, |e| e.kind == MoveKind::WallRun),
            invulnerable: stats.spawn_protected || me.map_or(false, |e| e.flags & FLAG_INVULN != 0),
            respawn_in_s: if dead || stats.respawn_in_s.is_some() { stats.respawn_in_s } else { None },
            ping_ms: stats.ping_ms,
            killfeed: stats.killfeed.clone(),
            show_scoreboard: stats.show_scoreboard,
            scoreboard: stats.scoreboard.clone(),
            dead,
            match_clock: stats.match_clock,
            match_map: stats.match_map,
            net_warn: net.bad_datagrams > stats.bad_datagrams_baseline,
            time,
            ..Default::default()
        };
        fx.apply(&mut s);
        s
    }
}

pub const HIT_MARK_TIME: f32 = 0.3;
pub const DAMAGE_TIME: f32 = 0.9;
pub const KILLFEED_TIME: f32 = 5.0;
pub const KILLFEED_MAX: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Banner {
    pub text: String,
    pub sub: Option<String>,
    pub color: BannerColor,
    /// Seconds shown so far / total; drawn faded over the last `BANNER_FADE`.
    pub age: f32,
    pub life: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerColor {
    Cyan,
    Orange,
    Gold,
    Magenta,
}

pub const BANNER_FADE: f32 = 0.35;
pub const TOAST_TIME: f32 = 1.6;

/// Transient, locally-timed feedback driven by combat events. Advance with `tick`, feed events in,
/// then `apply` onto the `HudState` each frame.
#[derive(Clone, Debug, Default)]
pub struct HudFx {
    hit: Option<(f32, DamageKind)>,
    hit_kill: bool,
    hit_amount: u8,
    hurt: Option<(f32, f32)>,
    hurt_frac: f32,
    banner: Option<Banner>,
    toast: Option<(String, f32)>,
}

impl HudFx {
    pub fn tick(&mut self, dt: f32) {
        self.hit = self.hit.map(|(t, k)| (t + dt, k)).filter(|(t, _)| *t < HIT_MARK_TIME);
        self.hurt = self.hurt.map(|(t, b)| (t + dt, b)).filter(|(t, _)| *t < DAMAGE_TIME);
        if let Some(b) = &mut self.banner {
            b.age += dt;
        }
        self.banner = self.banner.take().filter(|b| b.age < b.life);
        if let Some((_, t)) = &mut self.toast {
            *t += dt;
        }
        self.toast = self.toast.take().filter(|(_, t)| *t < TOAST_TIME);
    }

    fn show(&mut self, text: impl Into<String>, sub: Option<&str>, color: BannerColor, life: f32) {
        self.banner = Some(Banner { text: text.into(), sub: sub.map(String::from), color, age: 0.0, life });
    }

    /// Match flow from the sim's `MatchEvent`. Warmup shows 3-2-1 big, start is "FIGHT", end is "MATCH OVER"
    /// with the winner, next-map is "MAP n". `local_id` colours the end banner (gold = you won).
    pub fn on_match(&mut self, e: &MatchEvent, local_id: u16) {
        match *e {
            MatchEvent::Countdown { phase: PhaseKind::Warmup, seconds, .. } => self.countdown(seconds),
            MatchEvent::Countdown { .. } => {}
            MatchEvent::MatchStart { .. } => self.show("FIGHT", None, BannerColor::Orange, 1.5),
            MatchEvent::MatchEnd { winner, reason, .. } => {
                let why = match reason {
                    EndReason::FragLimit => "FRAG LIMIT",
                    EndReason::TimeLimit => "TIME LIMIT",
                };
                self.match_over(winner, why, local_id, |id| format!("PLAYER {id}"));
            }
            MatchEvent::NextMap { map, .. } => self.show(format!("MAP {}", map + 1), Some("LOADING"), BannerColor::Cyan, 2.0),
        }
    }

    fn countdown(&mut self, seconds: u32) {
        if (1..=3).contains(&seconds) {
            self.show(seconds.to_string(), Some("GET READY"), BannerColor::Cyan, 1.0);
        }
    }

    fn match_over(&mut self, winner: Option<u16>, why: &str, local_id: u16, name: impl Fn(u16) -> String) {
        let (sub, c) = match winner {
            Some(w) if w == local_id => (format!("WINNER: YOU \u{b7} {why}"), BannerColor::Gold),
            Some(w) => (format!("WINNER: {} \u{b7} {why}", name(w)), BannerColor::Magenta),
            None => (format!("DRAW \u{b7} {why}"), BannerColor::Cyan),
        };
        self.show("MATCH OVER", Some(&sub), c, 8.0);
    }

    /// Reliable server events: match banners, local pickup toast. Kills go through `KillEntry::from_net`.
    /// Warmup countdown only (intermission shows the clock and the final scoreboard instead).
    pub fn on_net_event(&mut self, e: &NetEvent, local_id: u16, name: impl Fn(u16) -> String) {
        match *e {
            NetEvent::MatchCountdown { phase, seconds, .. } if phase != PHASE_LIVE && phase != PHASE_INTERMISSION => {
                self.countdown(seconds as u32)
            }
            NetEvent::MatchStart { .. } => self.show("FIGHT", None, BannerColor::Orange, 1.5),
            NetEvent::MatchEnd { winner, reason, .. } => {
                let why = if reason == 0 { "FRAG LIMIT" } else { "TIME LIMIT" };
                self.match_over((winner != 0).then_some(winner), why, local_id, name);
            }
            // The only pickup kind is the grid beam (wire kind 0); the taken event doesn't carry a kind.
            NetEvent::PickupTaken { player, .. } if player == local_id => self.toast = Some(("+ GRID BEAM".into(), 0.0)),
            _ => {}
        }
    }

    /// Local player's own combat events (`Damage` = outgoing -> hit marker).
    pub fn on_combat(&mut self, e: &CombatEvent) {
        if let CombatEvent::Damage { kind, .. } = e {
            self.on_hit(*kind, false);
        }
    }

    /// Dealt hit from the server's own-hit message; `killed` makes the marker big and gold.
    pub fn on_hit(&mut self, kind: DamageKind, killed: bool) {
        self.on_hit_amount(kind, killed, 0);
    }

    /// `on_hit` plus the damage dealt (shown as a number next to the reticle).
    pub fn on_hit_amount(&mut self, kind: DamageKind, killed: bool, amount: u8) {
        self.hit = Some((0.0, kind));
        self.hit_kill = killed;
        self.hit_amount = amount;
    }

    /// `bearing` from `bearing_to`: radians relative to view, 0 = ahead, +ve = clockwise (to the right).
    pub fn on_hurt(&mut self, bearing: f32) {
        self.on_hurt_amount(bearing, 0);
    }

    /// `on_hurt` plus the damage taken in points; bigger hits draw a wider wedge.
    pub fn on_hurt_amount(&mut self, bearing: f32, amount: u8) {
        self.hurt = Some((0.0, bearing));
        self.hurt_frac = (amount as f32 / shinobi_sim::pvp::MAX_HP).clamp(0.0, 1.0);
    }

    pub fn apply(&self, s: &mut HudState) {
        s.since_hit = self.hit.map(|h| h.0);
        s.hit_kind = self.hit.map(|h| h.1);
        s.hit_kill = self.hit.is_some() && self.hit_kill;
        s.since_damage = self.hurt;
        s.hit_amount = if self.hit.is_some() { self.hit_amount } else { 0 };
        s.damage_frac = if self.hurt.is_some() { self.hurt_frac } else { 0.0 };
        s.banner = self.banner.clone();
        s.toast = self.toast.clone();
    }
}

/// Server ticks per second (matches `shinobi_proto::TICK_HZ`).
const TICK_HZ: f32 = 60.0;

impl LocalStats {
    /// hp, respawn countdown and spawn protection from the sim's `Vitals` at `tick`. Cooldowns, killfeed and
    /// scoreboard are set separately.
    pub fn apply_vitals(&mut self, v: &Vitals, tick: u32) {
        self.hp = v.hp_frac();
        self.respawn_in_s = v.dead_until_tick.map(|t| t.saturating_sub(tick) as f32 / TICK_HZ);
        self.spawn_protected = v.alive() && v.spawn_protected(tick);
    }
}

impl LocalStats {
    /// Clock for the current phase; seconds round up so "0:01" shows until it really ends.
    pub fn apply_match(&mut self, m: &MatchState, tick: u32) {
        self.match_clock = Some((m.kind(), m.ticks_left(tick).map(|t| t.div_ceil(TICK_HZ as u32))));
    }
}

/// Scoreboard rows from the sim's `MatchState::scoreboard` (already sorted); dead players are shown dim by
/// the renderer via their row (alive flag is not drawn yet).
pub fn score_rows_from_entries(
    entries: &[ScoreEntry],
    local_id: u16,
    name: impl Fn(u16) -> String,
    ping_ms: impl Fn(u16) -> u16,
) -> Vec<ScoreRow> {
    entries
        .iter()
        .map(|e| ScoreRow { name: name(e.id), kills: e.kills, deaths: e.deaths, ping_ms: ping_ms(e.id), local: e.id == local_id, alive: e.alive, team: None })
        .collect()
}

/// Scoreboard rows from `(player id, vitals)` pairs; `ping_ms` and `name` are looked up by player id.
pub fn score_rows<'a>(
    players: impl IntoIterator<Item = (u16, &'a Vitals)>,
    local_id: u16,
    name: impl Fn(u16) -> String,
    ping_ms: impl Fn(u16) -> u16,
) -> Vec<ScoreRow> {
    players
        .into_iter()
        .map(|(id, v)| ScoreRow { name: name(id), kills: v.kills, deaths: v.deaths, ping_ms: ping_ms(id), local: id == local_id, alive: v.alive(), team: None })
        .collect()
}

/// Wire `KIND_*` -> weapon kind. Only shuriken / katana (and the legacy beam powerup) are weapons; anything
/// else is environmental and is shown as a void kill (`is_void`), never as a weapon.
pub fn damage_kind(k: u8) -> DamageKind {
    match k {
        KIND_SHURIKEN => DamageKind::Shuriken,
        KIND_GRID_BEAM => DamageKind::GridBeam,
        _ => DamageKind::Slash,
    }
}

/// Environmental damage source: anything that is not a weapon kind (obstacles, and any retired kind an old
/// server might still send, e.g. light walls).
pub fn is_void(k: u8) -> bool {
    !matches!(k, KIND_SHURIKEN | KIND_SLASH | KIND_GRID_BEAM)
}

impl KillEntry {
    /// `Some` for `NetEvent::Kill`. No killer (`killer == 0`) or an environmental kind is a void kill.
    pub fn from_net(e: &NetEvent, local_id: u16, name: impl Fn(u16) -> String) -> Option<Self> {
        let NetEvent::Kill { killer, victim, kind, .. } = *e else { return None };
        let void = killer == 0 || is_void(kind);
        Some(Self {
            killer: if void { String::new() } else { name(killer) },
            victim: name(victim),
            weapon: damage_kind(kind),
            age: 0.0,
            involves_local: (!void && killer == local_id) || victim == local_id,
            void,
            killer_team: None,
            victim_team: None,
        })
    }

    /// `Some` for `Kill` and `HazardKill`.
    pub fn from_pvp(e: &PvpEvent, local_id: u16, name: impl Fn(u16) -> String) -> Option<Self> {
        match *e {
            PvpEvent::Kill { killer, victim, kind, .. } => Some(Self {
                killer: name(killer),
                victim: name(victim),
                weapon: kind,
                age: 0.0,
                involves_local: killer == local_id || victim == local_id,
                void: false,
                killer_team: None,
                victim_team: None,
            }),
            // Hazards are environmental: void glyph, no killer, even if an old sim names a wall owner.
            PvpEvent::HazardKill { victim, .. } => Some(Self {
                killer: String::new(),
                victim: name(victim),
                weapon: DamageKind::Slash,
                age: 0.0,
                involves_local: victim == local_id,
                void: true,
                killer_team: None,
                victim_team: None,
            }),
            _ => None,
        }
    }
}

/// Append to the feed (newest last), drop expired entries, and age the rest by `dt`.
pub fn killfeed_tick(feed: &mut Vec<KillEntry>, dt: f32) {
    for k in feed.iter_mut() {
        k.age += dt;
    }
    feed.retain(|k| k.age < KILLFEED_TIME);
}

impl HudFx {
    /// Server PvP events. `Hit` by us -> hit marker; `Hit` on us -> damage direction, using
    /// `attacker_bearing(attacker_id)` (see `bearing_to`; return None if the attacker isn't visible,
    /// which draws nothing rather than a wrong arrow).
    pub fn on_pvp(&mut self, e: &PvpEvent, local_id: u16, attacker_bearing: impl FnOnce(u16) -> Option<f32>) {
        if let PvpEvent::Hit { attacker, victim, kind, .. } = *e {
            if attacker == local_id && victim != local_id {
                self.on_hit(kind, false);
            } else if victim == local_id {
                if let Some(b) = attacker_bearing(attacker) {
                    self.hurt = Some((0.0, b));
                }
            }
        }
    }
}

/// View-relative bearing of `src` from `me` (world XZ). Yaw convention matches the sim: yaw 0 faces -Z and
/// positive yaw turns toward -X. Result: 0 = dead ahead, +pi/2 = to the right.
pub fn bearing_to(me: [f32; 3], yaw: f32, src: [f32; 3]) -> f32 {
    let (dx, dz) = (src[0] - me[0], src[2] - me[2]);
    let world = (-dx).atan2(-dz); // yaw-space angle of the direction (0 = -Z, + toward -X)
    let rel = yaw - world; // + = source is clockwise (right) of view
    (rel + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// Radial shade in [0, max_a]; `r` = distance from center normalized so a corner = 1.
pub fn vignette_alpha(r: f32, start: f32, max_a: f32) -> f32 {
    if r <= start {
        return 0.0;
    }
    let t = ((r - start) / (1.0 - start)).clamp(0.0, 1.0);
    max_a * t * t * (3.0 - 2.0 * t) // smoothstep
}

pub struct Hud;

impl Hud {
    pub fn build(s: &HudState) -> Vec<Prim> {
        Self::build_with(s, &Palette::TRON)
    }

    pub fn build_with(s: &HudState, p: &Palette) -> Vec<Prim> {
        let mut out = Vec::with_capacity(48);
        let low_hp = s.hp < 0.3;
        // Low HP: slow amber->magenta pulse on the chrome, never a full-screen flash.
        let pulse = if low_hp { 0.75 + 0.25 * (s.time * 6.0).sin() } else { 1.0 };
        let chrome = if low_hp { p.magenta } else { p.cyan };
        let chrome = if s.invulnerable { p.gold } else { chrome };
        let chrome = if s.dead { p.magenta } else { chrome };

        // Low HP: heartbeat on the edge tint (two quick beats, then rest), never above the vignette cap.
        let beat = if low_hp && !s.dead {
            let ph = (s.time * 1.4).fract();
            let pk = |c: f32| (-((ph - c) / 0.05).powi(2)).exp();
            (pk(0.0) + 0.7 * pk(0.18)).min(1.0)
        } else {
            0.0
        };
        out.push(Prim::Vignette { start: VIGNETTE_START, max_a: VIGNETTE_MAX_A * (0.55 + 0.45 * beat), tint: chrome.a(1.0) });
        out.push(Prim::Scanlines { count: 240, scroll: (s.time * 0.05).fract(), a: 0.035 });

        // Visor rails: hairlines hugging the side edges, brow and chin.
        let rail = chrome.a(0.55 * pulse);
        for x in [RAIL_W * 0.5, 1.0 - RAIL_W * 0.5] {
            out.push(Prim::Line { a: (x, 0.12), b: (x, 0.88), color: rail, glow: 0.004 });
        }
        out.push(Prim::Line { a: (0.22, BROW_H), b: (0.78, BROW_H), color: rail, glow: 0.004 });
        // Chamfered corner ticks give the retro-arcade bezel feel.
        for (cx, dx) in [(SIDE_CLEAR, 1.0), (1.0 - SIDE_CLEAR, -1.0)] {
            out.push(Prim::Line { a: (cx, BROW_H), b: (cx - dx * 0.03, 0.0), color: rail, glow: 0.004 });
            out.push(Prim::Line { a: (cx, CHIN_TOP), b: (cx - dx * 0.03, 1.0), color: rail, glow: 0.004 });
        }
        out.push(Prim::Line { a: (SIDE_CLEAR, CHIN_TOP), b: (1.0 - SIDE_CLEAR, CHIN_TOP), color: rail, glow: 0.004 });

        // Chin: hp bar (left), ultimate bar (right), and between them three cooldown arcs with weapon glyphs:
        // shuriken, katana, dash. Cyan ring = ready, orange sweep = recharging.
        bar(&mut out, Rect::new(0.24, 0.93, 0.17, 0.014), s.hp, if low_hp { p.magenta } else { p.cyan }, pulse);
        bar(&mut out, Rect::new(0.59, 0.93, 0.17, 0.014), s.ult, p.gold, 1.0);
        for (x, ready, glyph) in [(0.45, s.shuriken_ready, Glyph::Shuriken), (0.5, s.slice_ready, Glyph::Katana), (0.55, s.dash_ready, Glyph::Dash)] {
            cooldown_arc(&mut out, (x, 0.94), 0.026, ready, glyph, p);
        }

        // Brow: ping, right end (inside the clear-column edge).
        if let Some(ms) = s.ping_ms {
            let c = if ms < 80 { p.cyan } else if ms < 150 { p.orange } else { p.magenta };
            let (c, warn) = if s.net_warn { (p.magenta, "!") } else { (c, "") };
            out.push(Prim::Text { pos: (1.0 - SIDE_CLEAR - 0.01, 0.045), text: format!("{ms} MS{warn}"), size: 0.022, color: c.a(0.85), align: Align::Right });
        }
        if s.dead {
            out.push(Prim::Text { pos: (0.5, 0.05), text: "ELIMINATED".into(), size: 0.034, color: p.magenta.a(0.9), align: Align::Center });
            if let Some(t) = s.respawn_in_s {
                out.push(Prim::Text { pos: (0.5, 0.62), text: format!("RESPAWN {:.1}", t.max(0.0)), size: 0.04, color: p.orange.a(0.9), align: Align::Center });
            }
        }

        // Brow: wall-run chevron only when relevant (keeps the top quiet).
        if s.wall_run {
            out.push(Prim::Rect { r: Rect::new(0.47, 0.03, 0.06, 0.008), color: p.cyan.a(0.8), glow: 0.006 });
        }

        // Center reticle: four short ticks with a large open gap so targets stay visible.
        let spread = s.spread.clamp(0.0, 1.0);
        let (cx, cy, gap, len) = (0.5, 0.5, 0.012 + 0.03 * spread, 0.014);
        let ret = p.cyan.a(0.85);
        out.push(Prim::Line { a: (cx - gap - len, cy), b: (cx - gap, cy), color: ret, glow: 0.003 });
        out.push(Prim::Line { a: (cx + gap, cy), b: (cx + gap + len, cy), color: ret, glow: 0.003 });
        out.push(Prim::Line { a: (cx, cy + gap), b: (cx, cy + gap + len), color: ret, glow: 0.003 });

        // Hit marker: X at the centre. A dark under-stroke keeps it readable over bright scenery; colour says
        // what hit (cyan shuriken / orange slash / magenta beam), a kill is a long gold X with a ring, and a
        // slash adds a short horizontal tick so it differs from a shuriken hit without relying on colour.
        if let Some(t) = s.since_hit.filter(|t| *t < HIT_MARK_TIME) {
            let f = 1.0 - t / HIT_MARK_TIME;
            let kind_col = s.hit_kind.map_or(p.gold, |k| p.for_damage(k));
            let (arm, col) = if s.hit_kill { (0.035, p.gold) } else { (0.022, kind_col) };
            let d = 0.012 + 0.016 * (1.0 - f);
            let punch = f.sqrt();
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let (a, b) = ((cx + sx * d, cy + sy * d), (cx + sx * (d + arm), cy + sy * (d + arm)));
                out.push(Prim::Line { a, b, color: p.bg.a(0.85 * punch), glow: 0.014 });
                out.push(Prim::Line { a, b, color: col.a(punch), glow: 0.006 });
            }
            if s.hit_kill {
                out.push(Prim::Ring { c: (cx, cy), r: 0.03 + 0.02 * (1.0 - f), color: p.gold.a(0.7 * f), fill: false, glow: 0.008 });
            } else if s.hit_kind == Some(DamageKind::Slash) {
                for sx in [-1.0f32, 1.0] {
                    out.push(Prim::Line { a: (cx + sx * 0.012, cy), b: (cx + sx * 0.03, cy), color: col.a(punch), glow: 0.006 });
                }
            }
            // damage number drifting up from the reticle
            if s.hit_amount > 0 {
                out.push(Prim::Text {
                    pos: (cx, cy - 0.05 - 0.03 * (1.0 - f)),
                    text: s.hit_amount.to_string(),
                    size: if s.hit_kill { 0.032 } else { 0.026 },
                    color: col.a(f),
                    align: Align::Center,
                });
            }
        }

        // Damage direction: short arc tick on a ring of radius 0.09 around center (inside clear zone).
        if let Some((t, bearing)) = s.since_damage.filter(|(t, _)| *t < DAMAGE_TIME) {
            let f = 1.0 - t / DAMAGE_TIME;
            // red screen flash + a thick wedge of ticks pointing at the attacker (far outside the reticle)
            out.push(Prim::Vignette { start: 0.35, max_a: DAMAGE_FLASH_A * f, tint: p.magenta.a(1.0) });
            // wedge width and tick count grow with the size of the hit (unknown size draws a mid-size wedge)
            let frac = if s.damage_frac > 0.0 { s.damage_frac } else { 0.3 };
            let half = 0.1 + 0.5 * frac; // radians either side of the bearing
            let (r0, r1) = (0.16, 0.24 + 0.04 * frac);
            let n = 3 + (frac * 8.0).round() as i32;
            for k in 0..n {
                let u = if n > 1 { k as f32 / (n - 1) as f32 * 2.0 - 1.0 } else { 0.0 };
                let (sn, cs) = (bearing + u * half).sin_cos();
                let w = 1.0 - u.abs() * 0.6;
                // the centre tick reaches furthest: a clear arrowhead pointing at the attacker
                let reach = r1 + 0.03 * (1.0 - u.abs());
                out.push(Prim::Line {
                    a: (cx + sn * r0 * 0.5625, cy - cs * r0),
                    b: (cx + sn * reach * 0.5625, cy - cs * reach),
                    color: p.magenta.a(f * w),
                    glow: 0.012,
                });
            }
        }

        if let Some((phase, secs)) = s.match_clock {
            let time = secs.map(|t| format!("{}:{:02}", t / 60, t % 60));
            let (text, col) = match (phase, time) {
                (PhaseKind::Live, Some(t)) => (t, if secs.map_or(false, |t| t <= 30) { p.orange } else { p.cyan }),
                (PhaseKind::Live, None) => ("LIVE".into(), p.cyan),
                (PhaseKind::Warmup, Some(t)) => (format!("WARMUP {t}"), p.orange),
                (PhaseKind::Warmup, None) => ("WAITING FOR PLAYERS".into(), p.orange),
                (PhaseKind::Intermission, Some(t)) => (format!("NEXT MATCH {t}"), p.magenta),
                (PhaseKind::Intermission, None) => ("INTERMISSION".into(), p.magenta),
            };
            out.push(Prim::Text { pos: (SIDE_CLEAR + 0.01, 0.045), text, size: 0.026, color: col.a(0.9), align: Align::Left });
        }
        if let Some(b) = &s.banner {
            let fade = ((b.life - b.age) / BANNER_FADE).clamp(0.0, 1.0);
            let col = match b.color {
                BannerColor::Cyan => p.cyan,
                BannerColor::Orange => p.orange,
                BannerColor::Gold => p.gold,
                BannerColor::Magenta => p.magenta,
            };
            out.push(Prim::Text { pos: (0.5, 0.30), text: b.text.clone(), size: 0.09, color: col.a(fade), align: Align::Center });
            if let Some(sub) = &b.sub {
                out.push(Prim::Text { pos: (0.5, 0.35), text: sub.clone(), size: 0.03, color: p.text.a(0.8 * fade), align: Align::Center });
            }
        }
        if let Some((msg, t)) = &s.toast {
            let fade = ((TOAST_TIME - t) / 0.4).clamp(0.0, 1.0);
            out.push(Prim::Text { pos: (0.5, 0.84), text: msg.clone(), size: 0.03, color: p.gold.a(fade), align: Align::Center });
        }
        if let Some(ts) = s.team_scores {
            team_bar(&mut out, &ts, p);
        }
        for t in &s.ally_tags {
            ally_tag(&mut out, t, s.team_scores.map_or(0, |x| x.mine), p);
        }
        // Weapon slots under the cooldown arcs (only when the game supplies them).
        if let Some((have, max)) = s.shuriken_ammo {
            let c = if have == 0 { p.magenta } else { p.cyan };
            out.push(Prim::Text { pos: (0.45, 0.985), text: format!("{have}/{max}"), size: 0.02, color: c.a(0.9), align: Align::Center });
        }
        if let Some((blocks, ready)) = s.block_slot {
            cooldown_arc(&mut out, (0.6, 0.94), 0.026, ready, Glyph::Void, p);
            out.push(Prim::Text { pos: (0.6, 0.985), text: format!("x{blocks}"), size: 0.02, color: p.text.a(0.9), align: Align::Center });
        }
        killfeed(&mut out, s, p);
        if s.show_scoreboard {
            scoreboard(&mut out, s, p);
        }
        out
    }
}

/// Top-centre, newest first, max `KILLFEED_MAX` rows, fading over the last second. Sits below the brow and
/// inside x 0.30..0.70 so the side columns stay empty.
fn killfeed(out: &mut Vec<Prim>, s: &HudState, p: &Palette) {
    let rows = s.killfeed.iter().filter(|k| k.age < KILLFEED_TIME).rev().take(KILLFEED_MAX);
    for (i, k) in rows.enumerate() {
        let fade = ((KILLFEED_TIME - k.age) / 1.0).clamp(0.0, 1.0);
        let y = BROW_H + 0.035 + i as f32 * 0.034;
        let base = if k.involves_local { p.gold } else { p.text };
        let tag = p.for_damage(k.weapon);
        // team match: names take their team colour; a gold underline still marks rows that involve you
        let (kc, vc) = (k.killer_team.map_or(base, |t| p.team(t)), k.victim_team.map_or(base, |t| p.team(t)));
        if k.involves_local && (k.killer_team.is_some() || k.victim_team.is_some()) {
            out.push(Prim::Line { a: (0.40, y + 0.007), b: (0.60, y + 0.007), color: p.gold.a(0.7 * fade), glow: 0.003 });
        }
        if !k.void {
            out.push(Prim::Text { pos: (0.485, y), text: k.killer.clone(), size: 0.024, color: kc.a(0.9 * fade), align: Align::Right });
        }
        let glyph = if k.void { Glyph::Void } else { Glyph::of(k.weapon) };
        let col = if k.void { p.magenta } else { tag };
        weapon_icon(out, (0.5, y - 0.009), 0.011, glyph, col.a(fade));
        out.push(Prim::Text { pos: (0.515, y), text: k.victim.clone(), size: 0.024, color: vc.a(0.9 * fade), align: Align::Left });
    }
}

/// Top-centre team score bar: cyan score left, orange right, each side's bar fills outward from the middle
/// toward the kill limit. The local team's side is brighter. Inside x 0.36..0.64 (clear of the side columns).
fn team_bar(out: &mut Vec<Prim>, ts: &TeamScores, p: &Palette) {
    let (cx, y, half) = (0.5, 0.032, 0.11);
    let lim = ts.limit.max(1) as f32;
    for team in 0..2u8 {
        let sgn = if team == 0 { -1.0 } else { 1.0 };
        let col = p.team(team);
        let mine = ts.mine == team;
        let f = (ts.scores[team as usize] as f32 / lim).clamp(0.0, 1.0);
        let x0: f32 = cx + sgn * 0.035;
        let track = |frac: f32| Rect::new(x0.min(x0 + sgn * half * frac), y, half * frac, 0.01);
        out.push(Prim::Rect { r: track(1.0), color: col.a(0.14), glow: 0.0 });
        if f > 0.0 {
            out.push(Prim::Rect { r: track(f), color: col.a(if mine { 0.95 } else { 0.7 }), glow: if mine { 0.008 } else { 0.004 } });
        }
        out.push(Prim::Text {
            pos: (cx + sgn * 0.02, y + 0.012),
            text: ts.scores[team as usize].to_string(),
            size: if mine { 0.04 } else { 0.034 },
            color: col.a(if mine { 1.0 } else { 0.85 }),
            align: if team == 0 { Align::Right } else { Align::Left },
        });
    }
    out.push(Prim::Text { pos: (cx, y + 0.026), text: format!("FIRST TO {}", ts.limit), size: 0.016, color: p.text.a(0.5), align: Align::Center });
}

/// Ally marker: team-coloured diamond with the name above and distance below. Clamped tags (ally off screen or
/// behind) are dimmer so a real on-screen position is never confused with an edge hint.
fn ally_tag(out: &mut Vec<Prim>, t: &AllyTag, my_team: u8, p: &Palette) {
    let col = p.team(my_team);
    let near = (1.0 - (t.dist - 15.0) / 60.0).clamp(0.45, 1.0);
    let a = near * if t.clamped { 0.55 } else { 1.0 };
    let (x, y) = t.pos;
    let r = 0.011;
    let pts = [(x, y - r), (x + r * 0.5625, y), (x, y + r), (x - r * 0.5625, y)];
    for k in 0..4 {
        out.push(Prim::Line { a: pts[k], b: pts[(k + 1) % 4], color: col.a(a), glow: 0.005 });
    }
    out.push(Prim::Text { pos: (x, y - r - 0.008), text: t.name.clone(), size: 0.02, color: col.a(a), align: Align::Center });
    if !t.clamped {
        out.push(Prim::Text { pos: (x, y + r + 0.022), text: format!("{:.0}M", t.dist), size: 0.016, color: p.text.a(0.6 * a), align: Align::Center });
    }
}

/// Project a world point to normalized screen space: `(x, y, distance)` with y down, `None` when behind the
/// camera. Aim convention matches the camera: yaw 0 faces -Z (positive turns toward -X), positive pitch looks up.
/// `fov_y_deg` is the vertical field of view.
pub fn project(eye: [f32; 3], yaw: f32, pitch: f32, fov_y_deg: f32, aspect: f32, p: [f32; 3]) -> Option<(f32, f32, f32)> {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let fwd = [-sy * cp, sp, -cy * cp];
    let right = [cy, 0.0, -sy];
    let up = [right[1] * fwd[2] - right[2] * fwd[1], right[2] * fwd[0] - right[0] * fwd[2], right[0] * fwd[1] - right[1] * fwd[0]];
    let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
    let dot = |a: [f32; 3]| a[0] * d[0] + a[1] * d[1] + a[2] * d[2];
    let z = dot(fwd);
    if z <= 0.05 {
        return None;
    }
    let t = (fov_y_deg.to_radians() * 0.5).tan();
    let nx = dot(right) / (z * t * aspect.max(0.1));
    let ny = dot(up) / (z * t);
    Some((0.5 + 0.5 * nx, 0.5 - 0.5 * ny, (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()))
}

/// Centred Tron panel (dark fill + grid + neon border). Free-for-all: one list sorted by kills desc, deaths asc.
/// Team match (rows carry a team): cyan block then orange block, each sorted the same way under a coloured header
/// with the team's total; the local team comes first.
pub(crate) fn scoreboard(out: &mut Vec<Prim>, s: &HudState, p: &Palette) {
    let sort = |v: &mut Vec<&ScoreRow>| v.sort_by(|a, b| b.kills.cmp(&a.kills).then(a.deaths.cmp(&b.deaths)));
    let teamed = s.scoreboard.iter().any(|r| r.team.is_some());
    // (header colour/label/total, rows) per block
    let mut blocks: Vec<(Option<(u8, u32)>, Vec<&ScoreRow>)> = Vec::new();
    if teamed {
        let first = s.team_scores.map_or(0, |t| t.mine);
        for team in [first, 1 - first.min(1)] {
            let mut v: Vec<&ScoreRow> = s.scoreboard.iter().filter(|r| r.team.map_or(false, |t| (t != 0) == (team != 0))).collect();
            let total = v.iter().map(|r| r.kills as u32).sum();
            sort(&mut v);
            v.truncate(8);
            blocks.push((Some((team, total)), v));
        }
    } else {
        let mut v: Vec<&ScoreRow> = s.scoreboard.iter().collect();
        sort(&mut v);
        v.truncate(12);
        blocks.push((None, v));
    }
    let n_rows: usize = blocks.iter().map(|b| b.1.len()).sum();
    let n_heads = blocks.iter().filter(|b| b.0.is_some()).count();
    let (w, rh) = (0.5, if teamed { 0.036 } else { 0.04 });
    let h = 0.12 + rh * n_rows.max(1) as f32 + 0.05 * n_heads as f32;
    let r = Rect::new(0.25, 0.5 - h * 0.5, w, h);
    out.push(Prim::Rect { r, color: p.bg.a(0.78), glow: 0.0 });
    out.push(Prim::Grid { r, cell: 0.04, color: p.grid.a(0.35) });
    let (x1, y1) = (r.x + r.w, r.y + r.h);
    for (a, b) in [((r.x, r.y), (x1, r.y)), ((x1, r.y), (x1, y1)), ((x1, y1), (r.x, y1)), ((r.x, y1), (r.x, r.y))] {
        out.push(Prim::Line { a, b, color: p.cyan.a(0.8), glow: 0.006 });
    }
    // Title: map + phase + timer, e.g. "MAP 2 \u{b7} LIVE 2:05".
    if let Some(title) = scoreboard_title(s) {
        out.push(Prim::Text { pos: (0.5, r.y + 0.035), text: title, size: 0.028, color: p.cyan.a(0.95), align: Align::Center });
    }
    let cols = [(r.x + 0.05, Align::Left), (r.x + 0.32, Align::Right), (r.x + 0.40, Align::Right), (r.x + 0.48, Align::Right)];
    let hy = r.y + 0.075;
    for (&(x, al), t) in cols.iter().zip(["PLAYER", "K", "D", "PING"]) {
        out.push(Prim::Text { pos: (x, hy), text: t.into(), size: 0.022, color: p.orange.a(0.9), align: al });
    }
    let mut y = hy;
    for (head, rows) in &blocks {
        if let Some((team, total)) = head {
            y += 0.05;
            let col = p.team(*team);
            out.push(Prim::Rect { r: Rect::new(r.x + 0.006, y - 0.03, r.w - 0.012, 0.034), color: col.a(0.16), glow: 0.004 });
            out.push(Prim::Line { a: (r.x + 0.006, y - 0.03), b: (r.x + 0.006, y + 0.004), color: col.a(0.95), glow: 0.006 });
            let name = if *team == 0 { "CYAN" } else { "ORANGE" };
            out.push(Prim::Text { pos: (cols[0].0, y - 0.004), text: name.into(), size: 0.026, color: col.a(1.0), align: Align::Left });
            out.push(Prim::Text { pos: (cols[1].0, y - 0.004), text: total.to_string(), size: 0.026, color: col.a(1.0), align: Align::Right });
        }
        for row in rows {
            y += rh;
            let dim: f32 = if row.alive { 1.0 } else { 0.45 };
            if row.local {
                out.push(Prim::Rect { r: Rect::new(r.x + 0.006, y - 0.03, r.w - 0.012, rh - 0.002), color: p.gold.a(0.14), glow: 0.004 });
            }
            // alive dot: cyan = alive, magenta = down
            out.push(Prim::Ring { c: (r.x + 0.03, y - 0.011), r: 0.007, color: if row.alive { p.cyan } else { p.magenta }.a(dim.max(0.7)), fill: true, glow: 0.004 });
            let base = match row.team {
                Some(t) if !row.local => p.team(t),
                _ => if row.local { p.gold } else { p.text },
            };
            let c = base.a(dim);
            let cells = [row.name.clone(), row.kills.to_string(), row.deaths.to_string(), row.ping_ms.to_string()];
            for (&(x, al), t) in cols.iter().zip(cells) {
                out.push(Prim::Text { pos: (x, y), text: t, size: 0.024, color: c, align: al });
            }
        }
    }
}

fn scoreboard_title(s: &HudState) -> Option<String> {
    let (phase, secs) = s.match_clock?;
    let label = match phase {
        PhaseKind::Warmup => "WARMUP",
        PhaseKind::Live => "LIVE",
        PhaseKind::Intermission => "MATCH OVER",
    };
    let time = secs.map(|t| format!(" {}:{:02}", t / 60, t % 60)).unwrap_or_default();
    let map = s.match_map.map(|m| format!("MAP {} \u{b7} ", m + 1)).unwrap_or_default();
    Some(format!("{map}{label}{time}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Katana,
    Shuriken,
    Beam,
    Dash,
    /// Environmental kill: ring with a cross.
    Void,
}

impl Glyph {
    pub fn of(k: DamageKind) -> Self {
        match k {
            DamageKind::Slash => Glyph::Katana,
            DamageKind::Shuriken => Glyph::Shuriken,
            DamageKind::GridBeam => Glyph::Beam,
        }
    }
}

const ASPECT: f32 = 0.5625; // 9/16: x extents are scaled so glyphs stay round on a 16:9 screen

/// Line-drawn weapon icon centred on `c`, `h` = half-height in screen fractions. Lines only, so any backend
/// that draws `Prim::Line` gets it.
pub fn weapon_icon(out: &mut Vec<Prim>, c: (f32, f32), h: f32, g: Glyph, color: Rgba) {
    let pt = |x: f32, y: f32| (c.0 + x * h * ASPECT, c.1 + y * h);
    let mut line = |a: (f32, f32), b: (f32, f32)| out.push(Prim::Line { a: pt(a.0, a.1), b: pt(b.0, b.1), color, glow: 0.003 });
    match g {
        Glyph::Katana => {
            line((-0.9, 0.9), (0.9, -0.9)); // blade
            line((-0.55, 0.15), (-0.15, 0.55)); // guard
            line((-0.9, 0.9), (-0.65, 0.65)); // grip
        }
        Glyph::Shuriken => {
            // four-point star: tips on the axes, notches on the diagonals
            let tips = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)];
            let notch = [(0.3, -0.3), (0.3, 0.3), (-0.3, 0.3), (-0.3, -0.3)];
            for i in 0..4 {
                line(tips[i], notch[i]);
                line(notch[i], tips[(i + 1) % 4]);
            }
        }
        Glyph::Beam => {
            line((-1.0, 0.0), (1.0, 0.0));
            line((-0.6, -0.5), (-0.6, 0.5));
            line((0.6, -0.5), (0.6, 0.5));
        }
        Glyph::Dash => {
            line((-0.9, -0.7), (-0.2, 0.0));
            line((-0.2, 0.0), (-0.9, 0.7));
            line((0.1, -0.7), (0.8, 0.0));
            line((0.8, 0.0), (0.1, 0.7));
        }
        Glyph::Void => {
            // diamond with a cross: reads as "the map killed you"
            line((0.0, -1.0), (1.0, 0.0));
            line((1.0, 0.0), (0.0, 1.0));
            line((0.0, 1.0), (-1.0, 0.0));
            line((-1.0, 0.0), (0.0, -1.0));
            line((-0.45, -0.45), (0.45, 0.45));
            line((0.45, -0.45), (-0.45, 0.45));
        }
    }
}

/// Cooldown ring: dim full circle, plus a bright sweep from 12 o'clock covering `ready` (0..=1) of it.
/// Ready = cyan with glow; recharging = orange. The weapon glyph sits in the middle.
fn cooldown_arc(out: &mut Vec<Prim>, c: (f32, f32), r: f32, ready: f32, glyph: Glyph, p: &Palette) {
    const SEG: usize = 28;
    let ready = ready.clamp(0.0, 1.0);
    let done = ready >= 1.0;
    let col = if done { p.cyan } else { p.orange };
    let at = |i: usize| {
        let a = i as f32 / SEG as f32 * std::f32::consts::TAU;
        (c.0 + a.sin() * r * ASPECT, c.1 - a.cos() * r)
    };
    let lit = (ready * SEG as f32).round() as usize;
    for i in 0..SEG {
        let (a, b) = (at(i), at(i + 1));
        let on = i < lit;
        out.push(Prim::Line { a, b, color: col.a(if on { 0.95 } else { 0.18 }), glow: if on && done { 0.005 } else { 0.0 } });
    }
    weapon_icon(out, c, r * 0.45, glyph, col.a(if done { 0.95 } else { 0.55 }));
}

fn bar(out: &mut Vec<Prim>, r: Rect, frac: f32, color: Rgba, pulse: f32) {
    let f = frac.clamp(0.0, 1.0);
    out.push(Prim::Rect { r, color: color.a(0.12), glow: 0.0 }); // track
    if f > 0.0 {
        out.push(Prim::Rect { r: Rect::new(r.x, r.y, r.w * f, r.h), color: color.a(0.9 * pulse), glow: 0.006 });
    }
}

/// Bounding box of a primitive (ignoring glow), for layout checks.
pub fn bounds(p: &Prim) -> Option<Rect> {
    match *p {
        Prim::Rect { r, .. } => Some(r),
        Prim::Line { a, b, .. } => {
            let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
            let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        }
        Prim::Ring { c, r, .. } => Some(Rect::new(c.0 - r * 0.5625, c.1 - r, 2.0 * r * 0.5625, 2.0 * r)),
        Prim::Text { pos, ref text, size, align, .. } => {
            let w = text.chars().count() as f32 * size * 0.3; // rough mono advance, 9:16-corrected
            let x = match align {
                Align::Left => pos.0,
                Align::Center => pos.0 - w * 0.5,
                Align::Right => pos.0 - w,
            };
            Some(Rect::new(x, pos.1 - size, w, size))
        }
        Prim::Grid { r, .. } => Some(r),
        Prim::Vignette { .. } | Prim::Scanlines { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy() -> HudState {
        HudState {
            hp: 0.2,
            dash_ready: 0.5,
            shuriken_ready: 0.3,
            ult: 1.0,
            wall_run: true,
            since_hit: Some(0.05),
            hit_kind: Some(DamageKind::Slash),
            since_damage: Some((0.1, 1.2)),
            ping_ms: Some(42),
            killfeed: vec![
                KillEntry { killer: "Ronin".into(), victim: "Ghost".into(), weapon: DamageKind::Shuriken, age: 0.5, involves_local: true, void: false, killer_team: None, victim_team: None },
                KillEntry { killer: "Ghost".into(), victim: "Kage".into(), weapon: DamageKind::Slash, age: 9.0, involves_local: false, void: false, killer_team: None, victim_team: None },
            ],
            time: 3.0,
            ..Default::default()
        }
    }

    #[test]
    fn peripheral_columns_hold_only_hairline_rails() {
        for s in [HudState::default(), busy()] {
            for p in Hud::build(&s) {
                let Some(b) = bounds(&p) else { continue };
                let in_side = b.x < SIDE_CLEAR - 1e-4 || b.x + b.w > 1.0 - SIDE_CLEAR + 1e-4;
                if in_side {
                    // only the thin vertical rails or the bezel ticks that touch the clear-zone edge
                    let rail = b.w <= RAIL_W + 1e-4;
                    let tick = b.x >= SIDE_CLEAR - 0.031 && b.x + b.w <= 1.0 - SIDE_CLEAR + 0.031;
                    assert!(rail || tick, "content in peripheral column: {b:?}");
                }
            }
        }
    }

    #[test]
    fn center_reticle_leaves_crosshair_gap_open() {
        for p in Hud::build(&busy()) {
            if let Some(b) = bounds(&p) {
                let near = (b.x - 0.5).abs() < 0.0115 && (b.y - 0.5).abs() < 0.0115
                    && (b.x + b.w - 0.5).abs() < 0.0115 && (b.y + b.h - 0.5).abs() < 0.0115;
                assert!(!near, "center obstructed: {b:?}");
            }
        }
    }

    #[test]
    fn vignette_clear_inside_and_capped() {
        assert_eq!(vignette_alpha(0.0, VIGNETTE_START, VIGNETTE_MAX_A), 0.0);
        assert_eq!(vignette_alpha(VIGNETTE_START, VIGNETTE_START, VIGNETTE_MAX_A), 0.0);
        let edge = vignette_alpha(1.0, VIGNETTE_START, VIGNETTE_MAX_A);
        assert!((edge - VIGNETTE_MAX_A).abs() < 1e-6);
        assert!(vignette_alpha(2.0, VIGNETTE_START, VIGNETTE_MAX_A) <= VIGNETTE_MAX_A);
    }

    #[test]
    fn transient_markers_expire() {
        let base = HudState { hp: 1.0, dash_ready: 1.0, ..Default::default() };
        let n0 = Hud::build(&base).len();
        let hit = HudState { since_hit: Some(0.01), ..base.clone() };
        assert!(Hud::build(&hit).len() > n0);
        let old = HudState { since_hit: Some(HIT_MARK_TIME + 0.1), since_damage: Some((DAMAGE_TIME + 0.1, 0.0)), ..base };
        assert_eq!(Hud::build(&old).len(), n0);
    }

    #[test]
    fn low_hp_and_iframes_retint_chrome_not_fill_screen() {
        let ps = Hud::build(&busy());
        for p in &ps {
            if let Prim::Vignette { max_a, .. } = p {
                assert!(*max_a <= VIGNETTE_MAX_A.max(DAMAGE_FLASH_A));
            }
        }
    }

    #[test]
    fn killfeed_caps_expires_and_stays_off_the_sides() {
        let mk = |age| KillEntry { killer: "A".into(), victim: "B".into(), weapon: DamageKind::Slash, age, involves_local: false, void: false, killer_team: None, victim_team: None };
        let fresh: Vec<_> = (1..=KILLFEED_MAX + 1).map(|i| mk(i as f32 * 0.1)).collect();
        let s = HudState { killfeed: [fresh, vec![mk(KILLFEED_TIME + 1.0)]].concat(), ..Default::default() };
        let texts = Hud::build(&s).iter().filter(|p| matches!(p, Prim::Text { text, .. } if text == "A")).count();
        assert_eq!(texts, KILLFEED_MAX);
    }

    #[test]
    fn scoreboard_only_when_held_and_sorted_by_kills() {
        let row = |n: &str, k| ScoreRow { name: n.into(), kills: k, deaths: 0, ping_ms: 30, local: false, alive: true, team: None };
        let mut s = HudState { scoreboard: vec![row("low", 1), row("top", 9)], ..Default::default() };
        let names = |s: &HudState| -> Vec<String> {
            Hud::build(s).into_iter().filter_map(|p| match p { Prim::Text { text, .. } if text == "top" || text == "low" => Some(text), _ => None }).collect()
        };
        assert!(names(&s).is_empty());
        s.show_scoreboard = true;
        assert_eq!(names(&s), ["top", "low"]);
    }

    #[test]
    fn combat_damage_drives_hit_marker_and_it_decays() {
        let mut fx = HudFx::default();
        let ev = CombatEvent::Damage {
            target: 1,
            collider: shinobi_sim::rapier3d::prelude::ColliderHandle::invalid(),
            amount: 10.0,
            kind: DamageKind::GridBeam,
            point: shinobi_sim::rapier3d::prelude::Point::origin(),
        };
        fx.on_combat(&ev);
        let mut s = HudState::default();
        fx.apply(&mut s);
        assert_eq!(s.hit_kind, Some(DamageKind::GridBeam));
        assert_eq!(s.since_hit, Some(0.0));
        fx.tick(HIT_MARK_TIME + 0.01);
        fx.apply(&mut s);
        assert_eq!(s.since_hit, None);
    }

    #[test]
    fn bearing_left_right_ahead_behind() {
        let me = [0.0, 0.0, 0.0];
        let b = |yaw, src| bearing_to(me, yaw, src);
        assert!(b(0.0, [0.0, 0.0, -5.0]).abs() < 1e-5, "yaw 0 faces -Z: ahead");
        assert!((b(0.0, [5.0, 0.0, 0.0]) - std::f32::consts::FRAC_PI_2).abs() < 1e-5, "+X is right of -Z");
        assert!((b(0.0, [-5.0, 0.0, 0.0]) + std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!((b(0.0, [0.0, 0.0, 5.0]).abs() - std::f32::consts::PI).abs() < 1e-5);
        // turn toward -X (yaw +90deg): -X is now ahead
        assert!(b(std::f32::consts::FRAC_PI_2, [-5.0, 0.0, 0.0]).abs() < 1e-5);
    }

    #[test]
    fn from_frame_reads_local_status_from_the_newest_snapshot() {
        use shinobi_proto::datagram::{frame_snapshot_v3, Welcome};
        use shinobi_proto::snapshot::{encode_delta, EntityState, Snapshot};
        let mut net = NetClient::default();
        let w = Welcome { player_id: 7, server_tick: 100 };
        net.on_datagram(&w.encode(), 0.0).unwrap();
        let stats = LocalStats { hp: 0.8, dash_ready: 2.0, ping_ms: Some(40), ..Default::default() };
        let fx = HudFx::default();
        // connected but no snapshot yet: flags false, values clamped
        let s = HudState::from_frame(&net, &stats, &fx, 1.0);
        assert!(!s.dead && !s.wall_run && s.hp == 0.8 && s.dash_ready == 1.0 && s.ping_ms == Some(40));
        let me = EntityState { kind: MoveKind::WallRun, flags: FLAG_INVULN, ..Default::default() };
        let snap = Snapshot { tick: 102, entities: vec![(3, EntityState::default()), (7, me)] };
        let mut ev = shinobi_proto::bits::BitWriter::new();
        shinobi_sim::events::EventOutbox::default().write_pending(&mut ev, 64);
        net.on_datagram(&frame_snapshot_v3(&ev.finish(), &[], &[], &encode_delta(None, &snap)), 2.0).unwrap();
        let s = HudState::from_frame(&net, &stats, &fx, 1.0);
        assert!(s.wall_run && s.invulnerable && !s.dead);
        // another entity's flags never leak into the local HUD
        let snap2 = Snapshot { tick: 104, entities: vec![(3, EntityState { flags: FLAG_DEAD, ..Default::default() }), (7, me)] };
        let mut ev = shinobi_proto::bits::BitWriter::new();
        shinobi_sim::events::EventOutbox::default().write_pending(&mut ev, 64);
        net.on_datagram(&frame_snapshot_v3(&ev.finish(), &[], &[], &encode_delta(Some(&snap), &snap2)), 4.0).unwrap();
        assert!(!HudState::from_frame(&net, &stats, &fx, 1.0).dead);
    }

    #[test]
    fn dead_zeroes_hp_and_bad_datagrams_raise_the_ping_warning() {
        use shinobi_proto::datagram::{frame_snapshot_v3, Welcome};
        use shinobi_proto::snapshot::{encode_delta, EntityState, Snapshot};
        let mut net = NetClient::default();
        net.on_datagram(&Welcome { player_id: 1, server_tick: 0 }.encode(), 0.0).unwrap();
        let snap = Snapshot { tick: 2, entities: vec![(1, EntityState { flags: FLAG_DEAD, ..Default::default() })] };
        let mut ev = shinobi_proto::bits::BitWriter::new();
        shinobi_sim::events::EventOutbox::default().write_pending(&mut ev, 64);
        net.on_datagram(&frame_snapshot_v3(&ev.finish(), &[], &[], &encode_delta(None, &snap)), 1.0).unwrap();
        let _ = net.on_datagram(&[], 1.0); // bad datagram
        let stats = LocalStats { hp: 1.0, ping_ms: Some(30), ..Default::default() };
        let s = HudState::from_frame(&net, &stats, &HudFx::default(), 0.0);
        assert!(s.dead && s.hp == 0.0 && s.net_warn);
        let t = Hud::build(&s);
        assert!(t.iter().any(|p| matches!(p, Prim::Text { text, .. } if text == "ELIMINATED")));
        assert!(t.iter().any(|p| matches!(p, Prim::Text { text, .. } if text == "30 MS!")));
        let acked = LocalStats { bad_datagrams_baseline: net.bad_datagrams, ..stats };
        assert!(!HudState::from_frame(&net, &acked, &HudFx::default(), 0.0).net_warn);
    }

    fn vitals(hp: f32, dead_until: Option<u32>, invuln_until: u32, kills: u16) -> Vitals {
        Vitals {
            hp,
            max_hp: 100.0,
            kills,
            deaths: 2,
            dead_until_tick: dead_until,
            invuln_until_tick: invuln_until,
            spawn: [0.0; 3],
            hitbox: shinobi_sim::rapier3d::prelude::ColliderHandle::invalid(),
        }
    }

    #[test]
    fn vitals_drive_hp_respawn_countdown_and_spawn_protection() {
        let mut st = LocalStats::default();
        st.apply_vitals(&vitals(40.0, None, 500, 0), 450);
        assert_eq!((st.hp, st.respawn_in_s, st.spawn_protected), (0.4, None, true));
        st.apply_vitals(&vitals(40.0, None, 500, 0), 500);
        assert!(!st.spawn_protected, "protection ends at the tick");
        st.apply_vitals(&vitals(0.0, Some(780), 0, 0), 600);
        assert_eq!((st.hp, st.respawn_in_s, st.spawn_protected), (0.0, Some(3.0), false));
    }

    #[test]
    fn respawn_countdown_is_drawn_only_while_dead() {
        let mut net = NetClient::default();
        net.on_datagram(&shinobi_proto::datagram::Welcome { player_id: 1, server_tick: 0 }.encode(), 0.0).unwrap();
        let stats = LocalStats { respawn_in_s: Some(2.5), ..Default::default() };
        let alive = Hud::build(&HudState::from_frame(&net, &stats, &HudFx::default(), 0.0));
        assert!(!alive.iter().any(|p| matches!(p, Prim::Text { text, .. } if text.starts_with("RESPAWN"))));
        let mut s = HudState::from_frame(&net, &stats, &HudFx::default(), 0.0);
        s.dead = true;
        s.respawn_in_s = Some(2.5);
        assert!(Hud::build(&s).iter().any(|p| matches!(p, Prim::Text { text, .. } if text == "RESPAWN 2.5")));
    }

    #[test]
    fn pvp_kill_makes_a_killfeed_row_and_hits_route_by_who_was_hit() {
        let name = |id: u16| format!("P{id}");
        let kill = PvpEvent::Kill { tick: 10, killer: 1, victim: 2, kind: DamageKind::Slash };
        let k = KillEntry::from_pvp(&kill, 2, name).unwrap();
        assert_eq!((k.killer.as_str(), k.victim.as_str(), k.involves_local), ("P1", "P2", true));
        assert!(KillEntry::from_pvp(&PvpEvent::Respawn { tick: 1, player: 1 }, 1, name).is_none());
        assert!(!KillEntry::from_pvp(&kill, 9, name).unwrap().involves_local);

        let hit = |a, v| PvpEvent::Hit { tick: 1, attacker: a, victim: v, amount: 10.0, kind: DamageKind::Shuriken };
        let mut fx = HudFx::default();
        fx.on_pvp(&hit(5, 6), 5, |_| panic!("not hurt"));
        let mut s = HudState::default();
        fx.apply(&mut s);
        assert_eq!((s.hit_kind, s.since_damage), (Some(DamageKind::Shuriken), None));
        let mut fx = HudFx::default();
        fx.on_pvp(&hit(6, 5), 5, |a| (a == 6).then_some(1.0));
        fx.apply(&mut s);
        assert_eq!(s.since_damage, Some((0.0, 1.0)));
        let mut fx = HudFx::default();
        fx.on_pvp(&hit(6, 5), 5, |_| None);
        fx.apply(&mut s);
        assert_eq!(s.since_damage, None, "unknown attacker draws no arrow");
    }

    #[test]
    fn killfeed_ages_and_expires_and_scoreboard_rows_come_from_vitals() {
        let mut feed = vec![KillEntry { killer: "a".into(), victim: "b".into(), weapon: DamageKind::Slash, age: 0.0, involves_local: false, void: false, killer_team: None, victim_team: None }];
        killfeed_tick(&mut feed, 2.0);
        assert_eq!(feed[0].age, 2.0);
        killfeed_tick(&mut feed, KILLFEED_TIME);
        assert!(feed.is_empty());
        let (a, b) = (vitals(100.0, None, 0, 4), vitals(0.0, Some(9), 0, 1));
        let rows = score_rows([(1, &a), (2, &b)], 2, |i| format!("P{i}"), |i| i as u16 * 10);
        assert_eq!((rows[0].kills, rows[0].deaths, rows[0].local, rows[0].ping_ms), (4, 2, false, 10));
        assert!(rows[1].local && rows[1].name == "P2");
    }

    fn t(p: &[Prim], x: &str) -> bool {
        p.iter().any(|q| matches!(q, Prim::Text { text, .. } if text == x))
    }

    #[test]
    fn match_end_banner_names_the_winner_and_colours_by_local_result() {
        let end = |w| MatchEvent::MatchEnd { tick: 1, winner: w, reason: EndReason::FragLimit };
        for (w, sub, col) in [
            (Some(5), "WINNER: YOU \u{b7} FRAG LIMIT", BannerColor::Gold),
            (Some(6), "WINNER: PLAYER 6 \u{b7} FRAG LIMIT", BannerColor::Magenta),
            (None, "DRAW \u{b7} FRAG LIMIT", BannerColor::Cyan),
        ] {
            let mut fx = HudFx::default();
            fx.on_match(&end(w), 5);
            let mut s = HudState::default();
            fx.apply(&mut s);
            let b = s.banner.clone().unwrap();
            assert_eq!((b.text.as_str(), b.sub.as_deref(), b.color), ("MATCH OVER", Some(sub), col));
            let out = Hud::build(&s);
            assert!(t(&out, "MATCH OVER") && t(&out, sub));
        }
        let mut fx = HudFx::default();
        fx.on_match(&MatchEvent::MatchStart { tick: 0, map: 0 }, 1);
        fx.tick(1.0);
        let mut s = HudState::default();
        fx.apply(&mut s);
        assert!(t(&Hud::build(&s), "FIGHT"));
        fx.tick(0.6);
        fx.apply(&mut s);
        assert!(s.banner.is_none(), "FIGHT expires");
    }

    #[test]
    fn match_clock_formats_each_phase() {
        let mut s = HudState::default();
        for (clock, want) in [
            (Some((PhaseKind::Live, Some(125))), "2:05"),
            (Some((PhaseKind::Warmup, Some(7))), "WARMUP 0:07"),
            (Some((PhaseKind::Warmup, None)), "WAITING FOR PLAYERS"),
            (Some((PhaseKind::Intermission, Some(9))), "NEXT MATCH 0:09"),
        ] {
            s.match_clock = clock;
            assert!(t(&Hud::build(&s), want), "{want}");
        }
        s.match_clock = None;
        assert!(!t(&Hud::build(&s), "LIVE"));
    }

    #[test]
    fn apply_match_rounds_ticks_up_to_seconds() {
        let m = MatchState::default();
        let mut st = LocalStats::default();
        st.apply_match(&m, 0);
        assert_eq!(st.match_clock, Some((PhaseKind::Warmup, None)), "default warmup waits for players");
    }

    #[test]
    fn hazard_kills_feed_from_the_sim_events() {
        let name = |i: u16| format!("P{i}");
        let src = shinobi_sim::pvp::HazardSource::Obstacle(1);
        let wall_kill = PvpEvent::HazardKill { tick: 1, victim: 2, source: src, killer: Some(1) };
        let k = KillEntry::from_pvp(&wall_kill, 1, name).unwrap();
        assert_eq!((k.killer.as_str(), k.void, k.involves_local), ("", true, false), "even a named wall owner is just the void now");
        let world = PvpEvent::HazardKill { tick: 1, victim: 2, source: src, killer: None };
        let w = KillEntry::from_pvp(&world, 2, name).unwrap();
        assert!(w.void && w.involves_local, "victim is local");
        assert!(KillEntry::from_pvp(&PvpEvent::HazardHit { tick: 1, victim: 2, amount: 5.0, source: src }, 2, name).is_none());
    }

    #[test]
    fn sim_scoreboard_entries_convert_in_order() {
        let es = [
            ScoreEntry { id: 2, kills: 5, deaths: 1, alive: true },
            ScoreEntry { id: 1, kills: 3, deaths: 0, alive: false },
        ];
        let rows = score_rows_from_entries(&es, 1, |i| format!("P{i}"), |_| 25);
        assert_eq!((rows[0].name.as_str(), rows[0].kills, rows[0].local), ("P2", 5, false));
        assert!(rows[1].local && rows[1].deaths == 0);
    }

    fn ne_kill(killer: u16, victim: u16, kind: u8) -> NetEvent {
        NetEvent::Kill { tick: 1, killer, victim, kind }
    }

    #[test]
    fn net_kill_events_become_feed_rows_with_weapon_and_world_killer() {
        let name = |i: u16| format!("P{i}");
        let k = KillEntry::from_net(&ne_kill(3, 4, KIND_SLASH), 4, name).unwrap();
        assert_eq!((k.killer.as_str(), k.victim.as_str(), k.weapon, k.involves_local), ("P3", "P4", DamageKind::Slash, true));
        let w = KillEntry::from_net(&ne_kill(0, 4, 3), 3, name).unwrap();
        assert_eq!((w.killer.as_str(), w.void, w.involves_local), ("", true, false));
        assert!(!KillEntry::from_net(&ne_kill(0, 4, 3), 0, name).map_or(false, |k| k.involves_local && k.victim != "P4"));
        assert!(KillEntry::from_net(&NetEvent::Respawn { tick: 1, player: 3 }, 3, name).is_none());
    }

    #[test]
    fn net_match_events_drive_countdown_fight_and_end_banners() {
        let name = |i: u16| format!("P{i}");
        let mut fx = HudFx::default();
        let mut s = HudState::default();
        let cd = |phase, seconds| NetEvent::MatchCountdown { tick: 0, phase, seconds };
        use shinobi_proto::gameplay::PHASE_WARMUP;
        fx.on_net_event(&cd(PHASE_WARMUP, 7), 1, name);
        fx.apply(&mut s);
        assert!(s.banner.is_none(), "only 3-2-1 is big");
        for n in [3u8, 2, 1] {
            fx.on_net_event(&cd(PHASE_WARMUP, n), 1, name);
            fx.apply(&mut s);
            assert_eq!(s.banner.as_ref().unwrap().text, n.to_string());
        }
        fx.on_net_event(&cd(PHASE_INTERMISSION, 2), 1, name);
        fx.tick(5.0);
        fx.apply(&mut s);
        assert!(s.banner.is_none());
        fx.on_net_event(&NetEvent::MatchStart { tick: 0, map: 0 }, 1, name);
        fx.apply(&mut s);
        assert_eq!(s.banner.as_ref().unwrap().text, "FIGHT");
        fx.on_net_event(&NetEvent::MatchEnd { tick: 9, winner: 2, reason: 1 }, 1, name);
        fx.apply(&mut s);
        let b = s.banner.clone().unwrap();
        assert_eq!((b.text.as_str(), b.sub.as_deref()), ("MATCH OVER", Some("WINNER: P2 \u{b7} TIME LIMIT")));
        fx.on_net_event(&NetEvent::MatchEnd { tick: 9, winner: 0, reason: 0 }, 1, name);
        fx.apply(&mut s);
        assert!(s.banner.unwrap().sub.unwrap().starts_with("DRAW"));
    }

    #[test]
    fn pickup_toast_only_for_the_local_player_and_expires() {
        let name = |i: u16| format!("P{i}");
        let taken = |p| NetEvent::PickupTaken { tick: 1, id: 3, player: p };
        let mut fx = HudFx::default();
        let mut s = HudState::default();
        fx.on_net_event(&taken(8), 5, name);
        fx.on_net_event(&NetEvent::PickupSpawned { tick: 1, id: 3, pos: [0; 3], kind: 0 }, 5, name);
        fx.apply(&mut s);
        assert!(s.toast.is_none());
        fx.on_net_event(&taken(5), 5, name);
        fx.apply(&mut s);
        assert!(t(&Hud::build(&s), "+ GRID BEAM"));
        fx.tick(TOAST_TIME + 0.1);
        fx.apply(&mut s);
        assert!(s.toast.is_none());
    }

    #[test]
    fn kill_hit_marker_is_bigger_than_a_normal_hit() {
        let base = HudState { hp: 1.0, since_hit: Some(0.02), hit_kind: Some(DamageKind::Slash), ..Default::default() };
        let normal = Hud::build(&base);
        let kill = Hud::build(&HudState { hit_kill: true, ..base.clone() });
        assert!(kill.iter().any(|p| matches!(p, Prim::Ring { .. })) && !normal.iter().any(|p| matches!(p, Prim::Ring { .. })));
        let mut fx = HudFx::default();
        fx.on_hit(DamageKind::Slash, true);
        let mut s = HudState::default();
        fx.apply(&mut s);
        assert!(s.hit_kill);
        fx.on_hit(DamageKind::Slash, false);
        fx.apply(&mut s);
        assert!(!s.hit_kill);
        fx.tick(1.0);
        fx.apply(&mut s);
        assert!(!s.hit_kill && s.since_hit.is_none());
    }

    #[test]
    fn scoreboard_has_title_timer_alive_dots_and_highlights_the_local_row() {
        let row = |n: &str, k, alive, local| ScoreRow { name: n.into(), kills: k, deaths: 0, ping_ms: 10, local, alive, team: None };
        let s = HudState {
            show_scoreboard: true,
            match_clock: Some((PhaseKind::Live, Some(125))),
            match_map: Some(1),
            scoreboard: vec![row("me", 5, true, true), row("foe", 2, false, false)],
            ..Default::default()
        };
        let out = Hud::build(&s);
        assert!(t(&out, "MAP 2 \u{b7} LIVE 2:05"));
        let dots: Vec<_> = out.iter().filter_map(|p| if let Prim::Ring { color, fill: true, .. } = p { Some(*color) } else { None }).collect();
        assert_eq!(dots.len(), 2);
        let pal = Palette::TRON;
        assert_eq!((dots[0].0, dots[0].1), (pal.cyan.0, pal.cyan.1), "alive = cyan");
        assert_eq!((dots[1].0, dots[1].1), (pal.magenta.0, pal.magenta.1), "dead = magenta");
        let hl = |v: &[Prim]| v.iter().filter(|p| matches!(p, Prim::Rect { color, .. } if color.0 == pal.gold.0 && color.3 < 0.2)).count();
        let mut none_local = s.clone();
        none_local.scoreboard.iter_mut().for_each(|r| r.local = false);
        assert_eq!(hl(&out), hl(&Hud::build(&none_local)) + 1, "exactly one highlighted row");
        let mut s2 = s.clone();
        s2.match_clock = Some((PhaseKind::Intermission, Some(9)));
        s2.match_map = None;
        assert!(t(&Hud::build(&s2), "MATCH OVER 0:09"));
    }

    #[test]
    fn killfeed_keeps_five_rows() {
        let mk = |age| KillEntry { killer: "A".into(), victim: "B".into(), weapon: DamageKind::Slash, age, involves_local: false, void: false, killer_team: None, victim_team: None };
        let s = HudState { killfeed: (0..8).map(|i| mk(i as f32 * 0.1)).collect(), ..Default::default() };
        assert_eq!(Hud::build(&s).iter().filter(|p| matches!(p, Prim::Text { text, .. } if text == "A")).count(), 5);
    }

    fn lines(v: &[Prim]) -> usize {
        v.iter().filter(|p| matches!(p, Prim::Line { .. })).count()
    }

    #[test]
    fn unknown_or_retired_kinds_are_void_not_weapons() {
        for k in [3u8, 4, 9, 255] {
            let e = NetEvent::Kill { tick: 1, killer: 2, victim: 3, kind: k };
            let ke = KillEntry::from_net(&e, 3, |i| format!("P{i}")).unwrap();
            assert!(ke.void && ke.killer.is_empty(), "kind {k}");
        }
        let e = NetEvent::Kill { tick: 1, killer: 2, victim: 3, kind: KIND_SHURIKEN };
        assert!(!KillEntry::from_net(&e, 1, |i| format!("P{i}")).unwrap().void);
    }

    #[test]
    fn killfeed_draws_a_distinct_weapon_glyph_per_cause_and_no_killer_name_for_void() {
        let p = Palette::TRON;
        let row = |weapon, void| KillEntry { killer: "KKK".into(), victim: "VVV".into(), weapon, age: 0.0, involves_local: false, void, killer_team: None, victim_team: None };
        let draw = |k: KillEntry| Hud::build(&HudState { killfeed: vec![k], ..Default::default() });
        let base = lines(&Hud::build(&HudState::default()));
        let counts: Vec<_> = [
            draw(row(DamageKind::Slash, false)),
            draw(row(DamageKind::Shuriken, false)),
            draw(row(DamageKind::GridBeam, false)),
            draw(row(DamageKind::Slash, true)),
        ]
        .iter()
        .map(|d| lines(d) - base)
        .collect();
        assert_eq!(counts, [3, 8, 3, 6], "katana 3, shuriken 8, beam 3, void 6 strokes");
        assert!(!t(&draw(row(DamageKind::Slash, true)), "KKK") && t(&draw(row(DamageKind::Slash, true)), "VVV"));
        assert!(t(&draw(row(DamageKind::Slash, false)), "KKK"));
        let _ = p;
    }

    #[test]
    fn cooldown_arcs_sweep_with_progress_and_go_orange_while_recharging() {
        let lit = |s: &HudState| {
            Hud::build(s)
                .iter()
                .filter(|p| matches!(p, Prim::Line { color, .. } if color.3 > 0.9 && color.1 > 0.5 && color.2 < 0.2))
                .count()
        };
        let ready = HudState { shuriken_ready: 1.0, slice_ready: 1.0, dash_ready: 1.0, hp: 1.0, ..Default::default() };
        let empty = HudState { shuriken_ready: 0.0, slice_ready: 0.0, dash_ready: 0.0, hp: 1.0, ..Default::default() };
        let half = HudState { shuriken_ready: 0.5, slice_ready: 0.5, dash_ready: 0.5, hp: 1.0, ..Default::default() };
        assert_eq!(lit(&ready), 0, "ready arcs are cyan, not orange");
        assert!(lit(&empty) < lit(&half), "more sweep = more lit orange segments, glyph aside");
        // arcs live in the chin centre: nothing in the peripheral columns
        for q in Hud::build(&half) {
            if let Some(b) = bounds(&q) {
                assert!(b.x >= SIDE_CLEAR - 0.04 || b.w <= RAIL_W + 1e-4);
            }
        }
    }

    #[test]
    fn crosshair_gap_widens_with_spread() {
        let arm = |spread: f32| {
            let out = Hud::build(&HudState { hp: 1.0, spread, ..Default::default() });
            let r: Vec<_> = out
                .iter()
                .filter_map(|p| if let Prim::Line { a, b, .. } = p { Some((*a, *b)) } else { None })
                .filter(|(a, b)| (a.1 - 0.5).abs() < 1e-6 && (b.1 - 0.5).abs() < 1e-6)
                .collect();
            assert_eq!(r.len(), 2, "left + right ticks");
            r[1].0 .0 - 0.5 // right tick inner edge distance from centre
        };
        assert!(arm(1.0) > arm(0.0) + 0.02);
        assert!(arm(2.0) == arm(1.0), "clamped");
    }

    #[test]
    fn hit_number_wedge_size_low_hp_beat_and_weapon_slots() {
        let p = Palette::TRON;
        let texts = |s: &HudState| -> Vec<String> {
            Hud::build(s).iter().filter_map(|x| if let Prim::Text { text, .. } = x { Some(text.clone()) } else { None }).collect()
        };
        let hit = HudState { hp: 1.0, since_hit: Some(0.05), hit_kind: Some(DamageKind::Slash), hit_amount: 42, ..Default::default() };
        assert!(texts(&hit).contains(&"42".to_string()));
        assert!(!texts(&HudState { hit_amount: 0, ..hit.clone() }).contains(&"0".to_string()), "unknown amount hides the number");

        let wedge = |frac: f32| {
            let s = HudState { hp: 1.0, since_damage: Some((0.1, 0.0)), damage_frac: frac, ..Default::default() };
            Hud::build_with(&s, &p).iter().filter(|x| matches!(x, Prim::Line { glow, .. } if (*glow - 0.012).abs() < 1e-6)).count()
        };
        assert!(wedge(0.8) > wedge(0.1), "a bigger hit draws a wider wedge");

        for t in [0.0f32, 0.1, 0.18, 0.4, 0.7] {
            let s = HudState { hp: 0.1, time: t, ..Default::default() };
            for pr in Hud::build(&s) {
                if let Prim::Vignette { max_a, .. } = pr {
                    assert!(max_a <= VIGNETTE_MAX_A + 1e-6, "low-hp beat stays under the cap");
                }
            }
        }

        let slots = HudState { hp: 1.0, shuriken_ammo: Some((3, 5)), block_slot: Some((7, 1.0)), ..Default::default() };
        let t = texts(&slots);
        assert!(t.contains(&"3/5".to_string()) && t.contains(&"x7".to_string()));
    }

    #[test]
    fn projection_centres_ahead_flips_with_side_and_drops_behind() {
        let eye = [0.0, 1.5, 0.0];
        let (x, y, d) = project(eye, 0.0, 0.0, 90.0, 16.0 / 9.0, [0.0, 1.5, -10.0]).unwrap();
        assert!((x - 0.5).abs() < 1e-4 && (y - 0.5).abs() < 1e-4 && (d - 10.0).abs() < 1e-3);
        assert!(project(eye, 0.0, 0.0, 90.0, 1.0, [5.0, 1.5, -10.0]).unwrap().0 > 0.5, "+X is right of a -Z view");
        assert!(project(eye, 0.0, 0.0, 90.0, 1.0, [0.0, 4.5, -10.0]).unwrap().1 < 0.5, "higher is up the screen");
        assert!(project(eye, 0.0, 0.0, 90.0, 1.0, [0.0, 1.5, 10.0]).is_none(), "behind the camera");
        // turning to face the target (yaw positive turns toward -X) puts a -X target dead centre
        let (x, _, _) = project(eye, std::f32::consts::FRAC_PI_2, 0.0, 90.0, 1.0, [-10.0, 1.5, 0.0]).unwrap();
        assert!((x - 0.5).abs() < 1e-3);
    }

    #[test]
    fn team_bar_scoreboard_groups_and_ally_tags_respect_the_clear_columns() {
        let p = Palette::TRON;
        let row = |n: &str, k, team| ScoreRow { name: n.into(), kills: k, deaths: 0, ping_ms: 10, local: false, alive: true, team };
        let s = HudState {
            hp: 1.0,
            team_scores: Some(TeamScores { mine: 1, scores: [12, 30], limit: 50 }),
            ally_tags: vec![
                AllyTag { pos: (0.4, 0.4), name: "ALLY".into(), dist: 20.0, clamped: false },
                AllyTag { pos: (SIDE_CLEAR + 0.04, 0.5), name: "EDGE".into(), dist: 60.0, clamped: true },
            ],
            scoreboard: vec![row("a", 5, Some(0)), row("b", 9, Some(1)), row("c", 7, Some(0)), row("d", 1, Some(1))],
            show_scoreboard: true,
            ..Default::default()
        };
        let prims = Hud::build_with(&s, &p);
        let texts: Vec<String> = prims.iter().filter_map(|x| if let Prim::Text { text, .. } = x { Some(text.clone()) } else { None }).collect();
        for t in ["12", "30", "FIRST TO 50", "ALLY", "EDGE", "CYAN", "ORANGE"] {
            assert!(texts.contains(&t.to_string()), "{t}");
        }
        // local team (orange) block is listed first: its header precedes the cyan one
        assert!(texts.iter().position(|t| t == "ORANGE") < texts.iter().position(|t| t == "CYAN"));
        // block totals: cyan 5+7, orange 9+1
        assert!(texts.contains(&"12".to_string()) && texts.contains(&"10".to_string()));
        // no tag text lands in the side columns
        for x in &prims {
            if let Prim::Text { pos, text, .. } = x {
                if text == "ALLY" || text == "EDGE" || text == "12" || text == "30" {
                    assert!(pos.0 >= SIDE_CLEAR && pos.0 <= 1.0 - SIDE_CLEAR, "{text} in a side column");
                }
            }
        }
    }
}
