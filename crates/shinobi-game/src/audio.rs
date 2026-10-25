//! Game audio. Two layers:
//!   * `Cuer` (always compiled, pure, unit-tested): turns per-frame game state (`Frame`, built from `RenderView`) into
//!     sound cues and loop on/off states. No sound card, no bevy audio.
//!   * the playback backend (cargo feature `audio`, on by default): renders `synth` sounds to WAV at startup, plays
//!     cues with `bevy_audio` through the buses / limiter / ducker in `mixer`, positions other players' sounds in 3D
//!     with distance falloff, runs the ambient beds and the music slot (`music/` folder). No audio device = the game
//!     runs silent. Build without sound: `--no-default-features --features wayland`. See docs/AUDIO.md.

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use shinobi_client::client_game::RenderView;
use shinobi_sim::combat::aim_assist::LockEdge;
use shinobi_sim::combat::SlashKind;
use shinobi_sim::events::NetEvent;
use shinobi_proto::gameplay::KIND_SLASH;

use crate::hit_audio;
use crate::mixer::step_interval;
pub use crate::mixer::AudioSettings;
use crate::synth::Sound;

/// `/music next` and friends, queued by the console for the backend.
#[derive(Resource, Default)]
pub struct MusicCmds {
    pub next: bool,
}

/// One sound to play: `pos` = world position for 3D placement (None = plain, centred, our own).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub sound: Sound,
    pub pos: Option<[f32; 3]>,
    pub vol: f32,
    /// Playback speed (pitch): 1 = as rendered.
    pub speed: f32,
    /// How far the sound carries, metres (`mixer::falloff_range`).
    pub range: f32,
}

/// Cues queued by other systems (`destructible_audio`) for the playback backend; it drains this every frame. Capped.
#[derive(Resource, Default)]
pub struct ExtraCues(pub Vec<Cue>);

/// An "ear muffle" another system asks for (`storm_audio`: a lightning strike next to you): `(seconds, intensity 0..1)`.
/// The backend takes it each frame and feeds it to the same muffle a heavy hit uses.
#[derive(Resource, Default)]
pub struct ExtraMuffle(pub Option<(f32, f32)>);

impl ExtraCues {
    pub fn push(&mut self, c: Cue) {
        if self.0.len() >= 32 {
            self.0.remove(0);
        }
        self.0.push(c);
    }
}

impl Cue {
    pub fn at(sound: Sound, pos: [f32; 3], vol: f32) -> Self {
        Self { sound, pos: Some(pos), vol, speed: 1.0, range: crate::mixer::HEAR_RANGE }
    }
    fn flat(sound: Sound, vol: f32) -> Self {
        Self { sound, pos: None, vol, speed: 1.0, range: crate::mixer::HEAR_RANGE }
    }
    pub fn pitched(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }
    pub fn ranged(mut self, range: f32) -> Self {
        self.range = range;
        self
    }
}

/// What the cuer needs from one rendered frame.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub dt: f32,
    pub own_id: u16,
    pub lock_edges: Vec<LockEdge>,
    pub threw: bool,
    pub slash: bool,
    pub slash_kind: SlashKind,
    /// `strike_armed` of the local fx at the swing: 2 wall slash, 4 yank.
    pub strike: u8,
    pub slash_hit: Option<[f32; 3]>,
    pub slash_killed: bool,
    pub slash_miss: bool,
    pub grapple_attached: Option<[f32; 3]>,
    pub grapple_released: bool,
    pub grapple_missed: bool,
    pub grappling: bool,
    pub dashing: bool,
    pub wall_running: bool,
    pub grounded: bool,
    pub sliding: bool,
    pub speed: f32,
    pub jumped: bool,
    pub double_jumped: bool,
    pub landed: bool,
    pub landing_speed: f32,
    pub slide_started: bool,
    pub grinding: bool,
    pub grind_started: bool,
    /// Heavy-cut charge 0..1.
    pub slice_charge: f32,
    /// Flow tier (0..3, `combat::flow`): a tier-up gets its own stinger, distinct from the kill streak note.
    pub flow_tier: u32,
    /// Current BOUNTY (docs/bounty.md) as of this frame's board: `(id, live)`, `None` while nobody holds it.
    pub bounty_now: Option<(u16, bool)>,
    /// Star energy (charges, pool, locked out).
    pub star: Option<(f32, f32, bool)>,
    pub star_refused: bool,
    pub blocking: bool,
    /// Health 0..=1 (None before the first extras block).
    pub hp: Option<f32>,
    /// Hits we dealt: (damage, killed, headshot). Hits that only chipped a raised shield are in `dealt_blocked`.
    pub dealt: Vec<(u8, bool, bool)>,
    pub dealt_blocked: u32,
    /// Damage amounts we took (hits our shield absorbed or parried are counted in `absorbed` / `parried` instead).
    pub taken: Vec<u8>,
    /// One of the damage hits we took this frame was a headshot.
    pub taken_head: bool,
    /// Eye position (where near misses are measured from).
    pub eye: [f32; 3],
    /// Enemy projectiles in flight (stars, arrows): (position, owner). The backend drops allies.
    pub threats: Vec<([f32; 3], u16)>,
    /// Hits the shield took (reduced damage) and parried (zero damage) this frame.
    pub absorbed: u32,
    pub parried: u32,
    /// Discs in flight: (position, owner id).
    pub discs: Vec<([f32; 3], u16)>,
    /// Pieces: (owner, cell) and the cell edge in metres.
    pub blocks: Vec<(u16, [i32; 3])>,
    pub block_size: f32,
    /// Other players' ropes that are attached: (player, anchor).
    pub remote_ropes: Vec<(u16, [f32; 3])>,
    /// Other players moving on foot: (id, position, horizontal speed).
    pub remote_moving: Vec<(u16, [f32; 3], f32)>,
    /// Other players' deaths this frame, where they fell.
    pub remote_deaths: Vec<[f32; 3]>,
    // -- reliable events, own --
    pub my_kills: u32,
    /// Victims we killed this frame (almost always 0 or 1 entries): used to tell a bounty kill from a plain one.
    pub my_kill_victims: Vec<u16>,
    /// One of this frame's dealt hits was a kill, a bonus hit, with a katana, while the cloak button was held:
    /// a stealth kill (`cloak.rs`).
    pub stealth_kill: bool,
    /// We died (the killer id, 0 = the void / an obstacle).
    pub my_death: Option<u16>,
    pub respawned: bool,
    pub match_start: bool,
    /// Flash star effects this frame (`events::STAR_FX_*`, position), and a flash white-out that just hit us.
    pub star_fx: Vec<(u8, [f32; 3])>,
    /// Power-weapon moments (`power_pass::FX_*`, position, who) and our own predicted ones, and the seconds left on a fuda tag
    /// stuck to us (the countdown beeps).
    pub power_fx: Vec<(u8, [f32; 3], u16)>,
    pub power_own: Vec<shinobi_sim::power::PowerEvent>,
    pub tag_on_me: Option<f32>,
    /// A flash star dazed us: (strength 0..=1, seconds).
    pub blind: Option<(f32, f32)>,
    /// The grapple rope is an enemy hook (it pulls them toward us).
    pub pulling_enemy: bool,
    pub star_switched: bool,
    /// The match ended: did we win?
    pub match_end: Option<bool>,
    // -- UI --
    pub ui_click: bool,
    pub ui_back: bool,
}

impl Frame {
    pub fn from_view(v: &RenderView, own_id: u16, dt: f32) -> Frame {
        let f = &v.fx;
        let mut fr = Frame {
            dt,
            own_id,
            lock_edges: v.lock_edges.clone(),
            threw: f.threw.is_some(),
            slash: f.slash.is_some(),
            slash_kind: f.slash.map(|s| s.kind).unwrap_or_default(),
            strike: f.strike_armed,
            slash_hit: f.slash_hit.map(|h| h.point),
            slash_killed: f.slash_hit.map_or(false, |h| h.killed),
            slash_miss: f.slash_miss,
            grapple_attached: f.grapple_attached,
            grapple_released: f.grapple_released,
            pulling_enemy: f.grapple_pulling_enemy,
            grapple_missed: f.grapple_missed,
            grappling: f.grappling,
            dashing: f.dashing,
            wall_running: f.wall_running,
            grounded: f.grounded,
            sliding: f.sliding,
            speed: f.speed,
            jumped: f.jumped,
            double_jumped: f.double_jumped,
            landed: f.landed,
            landing_speed: f.landing_speed,
            slide_started: f.slide_started,
            grinding: f.grinding,
            grind_started: f.grind_started,
            slice_charge: f.slice_charge,
            flow_tier: f.flow_tier,
            bounty_now: v.board.as_ref().and_then(|b| (b.bounty != 0).then_some((b.bounty, b.bounty_live))),
            stealth_kill: f.cloak_held && v.hits.iter().any(|h| !h.taken && h.kill && h.bonus && h.kind == KIND_SLASH),
            star: v.star,
            star_refused: v.star_refused,
            blocking: v.blocking,
            hp: v.hp.map(|h| (h / 100.0).clamp(0.0, 1.0)),
            dealt: v.hits.iter().filter(|h| !h.taken && !h.guarded).map(|h| (h.amount, h.kill, h.head)).collect(),
            dealt_blocked: v.hits.iter().filter(|h| !h.taken && h.guarded && h.amount > 0).count() as u32,
            taken: v.hits.iter().filter(|h| h.taken && h.amount > 0 && !v.blocking && !h.guarded).map(|h| h.amount).collect(),
            taken_head: v.hits.iter().any(|h| h.taken && h.amount > 0 && h.head && !v.blocking && !h.guarded),
            eye: v.eye,
            threats: v
                .discs
                .iter()
                .enumerate()
                .map(|(i, (p, _))| (*p, v.disc_owners.get(i).copied().unwrap_or(0)))
                .chain(v.power_world.arrows.iter().map(|a| (a.pos, a.owner)))
                .filter(|(_, o)| *o != own_id)
                .collect(),
            absorbed: v.hits.iter().filter(|h| h.taken && h.amount > 0 && (v.blocking || h.guarded)).count() as u32,
            parried: v.hits.iter().filter(|h| h.taken && h.amount == 0 && !h.kill && h.other != 0).count() as u32,
            power_own: v.power_events.clone(),
            tag_on_me: v.power_world.tag_on(own_id).map(|t| t.fuse_left * shinobi_sim::power::TagCfg::default().fuse_s),
            discs: v.discs.iter().enumerate().map(|(i, (p, _))| (*p, v.disc_owners.get(i).copied().unwrap_or(0))).collect(),
            blocks: v.blocks.iter().map(|b| (b.owner, b.cell)).collect(),
            block_size: v.block_size,
            ..Default::default()
        };
        for e in &v.events {
            match e {
                NetEvent::Kill { killer, victim, .. } => {
                    if *victim as u16 == own_id {
                        fr.my_death = Some(*killer as u16);
                    } else if *killer as u16 == own_id {
                        fr.my_kills += 1;
                        fr.my_kill_victims.push(*victim as u16);
                    }
                }
                NetEvent::Respawn { player, .. } if *player as u16 == own_id => fr.respawned = true,
                NetEvent::MatchStart { .. } => fr.match_start = true,
                NetEvent::StarFx { what, pos, .. } => fr.star_fx.push((*what, shinobi_proto::quant::dq_pos3(*pos))),
                NetEvent::StarBlind { victim, strength_q, ticks, .. } if *victim == own_id => {
                    fr.blind = Some((*strength_q as f32 / 255.0, *ticks as f32 * shinobi_proto::TICK_DT));
                }
                NetEvent::PowerFx { what, pos, owner, .. } => fr.power_fx.push((*what, shinobi_proto::quant::dq_pos3(*pos), *owner)),
                NetEvent::MatchEnd { winner, .. } => fr.match_end = Some(*winner as u16 == own_id && own_id != 0),
                _ => {}
            }
        }
        fr
    }
}

/// Loops that should be playing right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loops {
    pub reel: bool,
    pub wall_run: bool,
}

/// The muffle of a flash daze: lasts the whole daze, harder for a stronger flash. `(secs, intensity)`.
pub fn dazed_muffle(strength: f32, secs: f32) -> (f32, f32) {
    (secs.max(0.0), 0.6 + 0.4 * strength.clamp(0.0, 1.0))
}

/// Damage at or above which a dealt hit uses the sharp "heavy hit" tick instead of the body thud.
pub const HEAVY_HIT: u8 = 60;
/// Shield hold time and refill time (mirrors the server's guard balance closely enough for a warning beep).
const GUARD_HOLD: f32 = 2.5;
const GUARD_REFILL: f32 = 3.0;
const GUARD_HIT_COST: f32 = 0.2;
const GUARD_LOW: f32 = 0.75;
const MAX_HP: f32 = shinobi_sim::pvp::MAX_HP;
const ROPE_PULSE: f32 = 0.5;
/// Seconds between scrape grains while grinding.
const GRIND_PULSE: f32 = 0.11;

/// Edge detector over frames: remembers last frame's discs / pieces / ropes and emits a `Cue` for each change.
#[derive(Default)]
pub struct Cuer {
    prev_dashing: bool,
    grind_t: f32,
    grind_alt: bool,
    prev_discs: Vec<([f32; 3], u16)>,
    prev_blocks: HashSet<(u16, [i32; 3])>,
    prev_ropes: HashSet<u16>,
    seen_first: bool,
    heart: f32,
    prev_locked: bool,
    prev_charges: f32,
    prev_charge: f32,
    streak: u32,
    step_t: f32,
    step_alt: bool,
    remote_steps: HashMap<u16, f32>,
    pulse_t: f32,
    tag_beep: f32,
    guard_spent: f32,
    guard_beep: f32,
    prev_threats: Vec<([f32; 3], u16)>,
    guard_broken: bool,
    /// `(seconds, intensity)` of the "ear muffle" this frame's hit asks for; read after `update`.
    pub muffle: Option<(f32, f32)>,
    /// A muffle a star effect asks for; it starts one frame later so the bang that causes it is not muffled itself.
    later_muffle: Option<(f32, f32)>,
    prev_pulling: bool,
    /// How hard this frame's events should push the music down (0..=1); read after `update`.
    pub duck: f32,
    /// Flow tier (`combat::flow`) as of the previous frame: a rise triggers the tier-up stinger.
    flow_tier_prev: u32,
    /// The BOUNTY `(id, live)` as of the START of the previous frame (docs/bounty.md), read before a kill this
    /// frame can drop it: mirrors `HudBridge::bounty_seen` so a kill on the just-cleared bounty still gets the beat.
    bounty_seen: Option<(u16, bool)>,
    /// Was the local player the LIVE bounty as of last frame: an edge into `true` is "you just became the bounty".
    was_bounty_live_me: bool,
}

impl Cuer {
    pub fn update(&mut self, f: &Frame) -> (Vec<Cue>, Loops) {
        let mut cues = Vec::new();
        let mut duck = 0.0f32;
        self.muffle = self.later_muffle.take();

        if f.ui_click {
            cues.push(Cue::flat(Sound::UiClick, 0.6));
        }
        if f.ui_back {
            cues.push(Cue::flat(Sound::UiBack, 0.6));
        }

        // the lock-on is silent but for one soft click when it snaps on (no acquire boop, no break fade)
        if f.lock_edges.iter().any(|e| matches!(e, LockEdge::Locked)) {
            cues.push(Cue::flat(Sound::LockClick, 0.5));
        }
        if f.threw {
            cues.push(Cue::flat(Sound::Throw, 0.8));
        }
        // the flash star, placed in the world where it bursts
        for (what, p) in &f.star_fx {
            use shinobi_sim::events::*;
            if *what == STAR_FX_FLASH_BURST {
                cues.push(Cue::at(Sound::FlashBang, *p, 1.0));
            }
        }
        // dazed by a flash: thin whine, and the rest of the world goes dull for as long as the daze lasts
        if let Some((strength, secs)) = f.blind {
            cues.push(Cue::flat(Sound::Tinnitus, 0.35 + 0.4 * strength.clamp(0.0, 1.0)));
            self.later_muffle = Some(dazed_muffle(strength, secs));
        }
        // power weapons (docs/powerups.md): existing synth sounds. Our own draw / loose / throw come from the prediction (no
        // latency), the server copies of them are skipped for us.
        for e in &f.power_own {
            use shinobi_sim::power::PowerEvent::*;
            match e {
                DrawStart => cues.push(Cue::flat(Sound::HeavyCharge, 0.5)),
                DrawCancel { .. } => cues.push(Cue::flat(Sound::Whiff, 0.4)),
                Loose { .. } => {
                    cues.push(Cue::flat(Sound::Throw, 1.0).pitched(0.55));
                    cues.push(Cue::flat(Sound::HeavyRelease, 0.5));
                    duck = duck.max(0.2);
                }
                TagThrow => cues.push(Cue::flat(Sound::Throw, 0.6).pitched(1.15)),
                TagDetonate => cues.push(Cue::flat(Sound::UiClick, 0.7)),
                Expired => cues.push(Cue::flat(Sound::Lockout, 0.5)),
                Spent => {}
            }
        }
        for (what, p, owner) in &f.power_fx {
            use shinobi_sim::power_pass::*;
            let mine = *owner == f.own_id;
            // the shield / hit cues below already voice an arrow that lands on us: do not double them
            let on_me = {
                let d = [p[0] - f.eye[0], p[1] - f.eye[1], p[2] - f.eye[2]];
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 9.0
            };
            if on_me
                && ((matches!(*what, FX_ARROW_BLOCKED) && f.absorbed > 0)
                    || (matches!(*what, FX_ARROW_PARRIED) && f.parried > 0)
                    || (matches!(*what, FX_ARROW_HIT | FX_ARROW_HEAD | FX_ARROW_SPLASH) && !f.taken.is_empty()))
            {
                continue;
            }
            let (s, v, pitch) = match *what {
                FX_PICKUP_YUMI | FX_PICKUP_TAG => {
                    cues.push(if mine { Cue::flat(Sound::Refill, 0.8) } else { Cue::flat(Sound::Execute, 0.45).pitched(1.0) });
                    continue;
                }
                FX_DRAW if !mine => (Sound::HeavyCharge, 0.45, 1.0),
                FX_LOOSE if !mine => (Sound::Throw, 0.9, 0.55),
                FX_TAG_THROW if !mine => (Sound::Throw, 0.5, 1.15),
                FX_ARROW_HIT => (Sound::HitBody, 1.0, 1.0),
                FX_ARROW_HEAD => (Sound::HitHard, 1.0, 1.0),
                // a near miss: the same body-hit cue as FX_ARROW_HIT, but quieter and a touch lower so it reads as
                // weaker than a real connect, not a second kind of impact.
                FX_ARROW_SPLASH => (Sound::HitBody, 0.55, 0.85),
                FX_ARROW_BLOCKED => (Sound::Block, 0.9, 1.0),
                FX_ARROW_PARRIED => (Sound::Parry, 1.0, 1.0),
                FX_ARROW_CUT | FX_TAG_CUT => (Sound::Clash, 0.8, 1.0),
                FX_ARROW_WALL => (Sound::Ricochet, 0.6, 0.8),
                FX_TAG_STUCK => (Sound::Print, 0.7, 1.0),
                FX_TAG_ARMED => (Sound::Refill, 0.45, 1.2),
                FX_TAG_BLAST => (Sound::AirSlam, 1.1, 0.8),
                FX_DROP => (Sound::Land, 0.6, 1.0),
                _ => continue,
            };
            cues.push(Cue::at(s, *p, v).pitched(pitch));
        }
        // a fuda tag stuck to us beeps faster and faster
        match f.tag_on_me {
            Some(secs) => {
                self.tag_beep -= f.dt;
                if self.tag_beep <= 0.0 {
                    cues.push(Cue::flat(Sound::RopePulse, 0.9).pitched(1.2));
                    self.tag_beep = 0.1 + 0.35 * (secs / 2.0).clamp(0.0, 1.0);
                }
            }
            None => self.tag_beep = 0.0,
        }
        if f.star_switched {
            cues.push(Cue::flat(Sound::UiClick, 0.5));
        }

        // star energy: buzz when empty, ping when it is back
        if let Some((charges, pool, locked)) = f.star {
            if locked && !self.prev_locked {
                cues.push(Cue::flat(Sound::Lockout, 0.7));
            } else if !locked && self.prev_locked {
                cues.push(Cue::flat(Sound::Refill, 0.7));
            } else if pool > 0.0 && charges >= pool - 1e-3 && self.prev_charges < pool - 1e-3 && !locked {
                cues.push(Cue::flat(Sound::Refill, 0.5));
            }
            self.prev_locked = locked;
            self.prev_charges = charges;
        }
        if f.star_refused {
            cues.push(Cue::flat(Sound::Lockout, 0.7));
        }

        // katana
        if f.slice_charge >= 0.05 && self.prev_charge < 0.05 {
            cues.push(Cue::flat(Sound::HeavyCharge, 0.6));
        }
        self.prev_charge = f.slice_charge;
        if f.slash {
            match f.slash_kind {
                SlashKind::Light => cues.push(Cue::flat(Sound::Swing, 0.8)),
                SlashKind::Heavy => {
                    cues.push(Cue::flat(Sound::HeavyRelease, 0.9));
                    duck = duck.max(0.3);
                }
                SlashKind::Air => cues.push(Cue::flat(Sound::Swing, 0.9).pitched(0.8)),
                SlashKind::Whirl => cues.push(Cue::flat(Sound::Whirl, 0.8)),
                SlashKind::Counter => {
                    cues.push(Cue::flat(Sound::CounterFlash, 0.8));
                    cues.push(Cue::flat(Sound::Swing, 0.7));
                }
            }
            match f.strike {
                2 => cues.push(Cue::flat(Sound::WallSlash, 0.8)),
                4 => cues.push(Cue::flat(Sound::GrappleYank, 0.9)),
                _ => {}
            }
        }
        if let Some(p) = f.slash_hit {
            if f.slash_kind == SlashKind::Air {
                cues.push(Cue::at(Sound::AirSlam, p, 1.0));
                duck = duck.max(0.4);
            } else {
                cues.push(Cue::at(Sound::Clash, p, 1.0));
            }
            if f.slash_killed {
                cues.push(Cue::at(Sound::Execute, p, 0.9));
                duck = duck.max(0.6);
            }
        }
        if f.slash_miss {
            cues.push(Cue::flat(Sound::Whiff, 0.5));
        }

        // grapple
        if f.grapple_missed {
            cues.push(Cue::flat(Sound::GrappleFire, 0.4));
        } else if f.grapple_attached.is_some() {
            cues.push(Cue::flat(Sound::GrappleFire, 0.7));
        }
        if let Some(p) = f.grapple_attached {
            cues.push(Cue::at(Sound::GrappleAttach, p, 1.0));
        }
        // the hook caught an enemy: a heavy whip crack on top of the attach
        if f.pulling_enemy && !self.prev_pulling {
            cues.push(Cue::flat(Sound::WhipCrack, 1.0));
            duck = duck.max(0.3);
        }
        self.prev_pulling = f.pulling_enemy;
        if f.grapple_released {
            cues.push(Cue::flat(Sound::GrappleRelease, 0.7));
        }
        if f.grappling {
            self.pulse_t -= f.dt;
            if self.pulse_t <= 0.0 {
                self.pulse_t = ROPE_PULSE;
                cues.push(Cue::flat(Sound::RopePulse, 0.35));
            }
        } else {
            self.pulse_t = 0.0;
        }

        // movement
        if f.dashing && !self.prev_dashing {
            cues.push(Cue::flat(Sound::Dash, 0.7));
        }
        self.prev_dashing = f.dashing;
        if f.slide_started {
            cues.push(Cue::flat(Sound::Slide, 0.6));
        }
        if f.grind_started {
            cues.push(Cue::flat(Sound::GrindStart, 0.7));
            self.grind_t = GRIND_PULSE;
        }
        // the scrape: a grain every GRIND_PULSE s while grinding, quicker and a touch higher the faster you go
        if f.grinding {
            self.grind_t -= f.dt;
            if self.grind_t <= 0.0 {
                self.grind_t = GRIND_PULSE;
                self.grind_alt = !self.grind_alt;
                cues.push(Cue::flat(Sound::GrindScrape, (0.2 + f.speed / 50.0).min(0.5)).pitched((0.9 + f.speed / 60.0) * if self.grind_alt { 1.0 } else { 0.93 }));
            }
        } else {
            self.grind_t = 0.0;
        }
        if f.jumped {
            cues.push(Cue::flat(Sound::Jump, 0.5));
        }
        if f.double_jumped {
            cues.push(Cue::flat(Sound::DoubleJump, 0.6));
        }
        if f.landed && f.landing_speed > 3.0 {
            let hard = f.landing_speed >= 12.0;
            cues.push(Cue::flat(if hard { Sound::LandHard } else { Sound::Land }, (0.3 + f.landing_speed / 25.0).min(1.0)));
        }
        // footsteps: stride-timed, alternating pitch, only while running on the ground
        let on_foot = f.grounded && !f.sliding && !f.grinding && !f.dashing && !f.wall_running;
        match step_interval(f.speed).filter(|_| on_foot) {
            Some(iv) => {
                self.step_t -= f.dt;
                if self.step_t <= 0.0 {
                    self.step_t = iv;
                    self.step_alt = !self.step_alt;
                    let loud = (0.25 + f.speed / 30.0).min(0.5);
                    cues.push(Cue::flat(Sound::Step, loud).pitched(if self.step_alt { 1.0 } else { 0.9 }));
                }
            }
            None => self.step_t = 0.0,
        }

        // hits we dealt, and the kill streak
        for (amount, kill, head) in &f.dealt {
            cues.push(Cue::flat(
                if *kill {
                    Sound::Kill
                } else if *amount >= HEAVY_HIT || *head {
                    Sound::HitHard
                } else {
                    Sound::HitBody
                },
                1.0,
            ));
            if *kill {
                duck = duck.max(0.5);
            }
            // a headshot kill also gets the sharp precision tick on top of the kill chime
            if *head && *kill {
                cues.push(Cue::flat(Sound::HitHard, 1.0));
            }
        }
        for _ in 0..f.my_kills {
            self.streak += 1;
            if self.streak >= 2 {
                cues.push(Cue::flat(Sound::Streak, 0.8).pitched(1.0 + 0.05 * (self.streak - 2).min(8) as f32));
            }
        }
        // a stealth kill (cloak.rs) gets its own harsher sting layered on top of the plain Kill chime already
        // pushed above, so it reads as "that was different" rather than replacing the normal kill confirmation
        if f.stealth_kill {
            cues.push(Cue::flat(Sound::StealthKill, 1.0));
            duck = duck.max(0.6);
        }
        // flow tier-up (combat::flow): a rise gets a bright ascending sparkle, distinct from the kill streak note
        if f.flow_tier > self.flow_tier_prev {
            cues.push(Cue::flat(Sound::FlowTierUp, 0.75).pitched(1.0 + 0.06 * (f.flow_tier - 1).min(2) as f32));
        }
        self.flow_tier_prev = f.flow_tier;
        // landed the kill on the marked BOUNTY (docs/bounty.md): the bounty as of the START of this frame (before
        // `f.bounty_now`, this frame's fresher board value, below) is what the kill is judged against, so a kill
        // that drops the bounty still gets the beat even though the board already cleared it this same frame.
        if f.my_kill_victims.iter().any(|v| self.bounty_seen.is_some_and(|(id, live)| id == *v && live)) {
            cues.push(Cue::flat(Sound::BountyKill, 1.0));
            duck = duck.max(0.6);
        }
        // becoming the BOUNTY: an edge into "live and it's me" (the HUD banner had no sound of its own before this)
        let now_live_me = f.bounty_now.is_some_and(|(id, live)| live && id == f.own_id && f.own_id != 0);
        if now_live_me && !self.was_bounty_live_me {
            cues.push(Cue::flat(Sound::BecomeBounty, 0.85));
        }
        self.was_bounty_live_me = now_live_me;
        self.bounty_seen = f.bounty_now;

        // the shield: a clang for every absorbed hit, a bright bell for a parry, a beep when it runs low
        for _ in 0..f.absorbed {
            cues.push(Cue::flat(Sound::Clang, 1.0));
            duck = duck.max(0.35);
            self.guard_spent = (self.guard_spent + GUARD_HIT_COST).min(1.0);
        }
        for _ in 0..f.dealt_blocked {
            // our blow rang off a shield: a dull clank, never the hit thud
            cues.push(Cue::flat(Sound::Clang, 0.55).pitched(0.8));
        }
        for _ in 0..f.parried {
            // bright chime: the bell, plus a higher octave copy on top
            cues.push(Cue::flat(Sound::Parry, 1.0));
            cues.push(Cue::flat(Sound::Parry, 0.5).pitched(0.75));
            duck = duck.max(0.4);
        }
        if f.blocking {
            self.guard_spent = (self.guard_spent + f.dt / GUARD_HOLD).min(1.0);
            self.guard_beep -= f.dt;
            if self.guard_spent >= GUARD_LOW && self.guard_beep <= 0.0 {
                self.guard_beep = 0.3;
                cues.push(Cue::flat(Sound::GuardLow, 0.5));
            }
        } else {
            self.guard_spent = (self.guard_spent - f.dt / GUARD_REFILL).max(0.0);
            self.guard_beep = 0.0;
        }
        // guard broken: the CLIENT's estimate of the guard ran out (the client does not expose the server's guard energy)
        if self.guard_spent >= 0.999 && !self.guard_broken && (f.absorbed > 0 || f.blocking) {
            self.guard_broken = true;
            cues.push(Cue::flat(Sound::GuardBreak, 1.0));
            duck = duck.max(0.6);
        } else if self.guard_spent < 0.5 {
            self.guard_broken = false;
        }

        // we took damage: ONE layered cue for the worst hit of the frame (bigger / headshot = harsher), the world dips
        if let Some(worst) = f.taken.iter().copied().max() {
            let (s, v, sp) = hit_audio::hurt_cue(worst, f.taken_head, MAX_HP);
            cues.push(Cue::flat(s, v).pitched(sp));
            let total: u32 = f.taken.iter().map(|a| *a as u32).sum();
            let total = total.min(255) as u8;
            if self.muffle.is_some() {
                self.later_muffle = self.muffle.or(self.later_muffle); // a star muffle due now waits behind the hit's
            }
            self.muffle = Some((hit_audio::muffle_secs(total, MAX_HP), hit_audio::intensity(total, MAX_HP)));
            duck = duck.max(0.4 + 0.4 * hit_audio::intensity(total, MAX_HP));
        }

        // enemy projectiles zipping past us
        for p in hit_audio::near_misses(&self.prev_threats, &f.threats, f.eye) {
            cues.push(Cue::at(Sound::Whoosh, p, 1.0).ranged(40.0));
        }
        self.prev_threats = f.threats.clone();

        // low-health heartbeat: faster and louder as health falls, silent when healthy / dead, restarts clean after a respawn
        if f.respawned || f.my_death.is_some() {
            self.heart = 0.0;
        }
        match f.hp.and_then(|h| hit_audio::heartbeat_period(h).map(|p| (h, p))) {
            Some((h, period)) => {
                self.heart -= f.dt;
                if self.heart <= 0.0 {
                    self.heart = period;
                    cues.push(Cue::flat(Sound::Heartbeat, hit_audio::heartbeat_vol(h)));
                }
            }
            None => self.heart = 0.0,
        }

        // life cycle and match stingers
        if let Some(killer) = f.my_death {
            self.streak = 0;
            if killer == 0 {
                cues.push(Cue::flat(Sound::VoidFall, 0.8));
            }
            cues.push(Cue::flat(Sound::DeathSting, 0.9));
            duck = duck.max(0.8);
        }
        if f.respawned {
            cues.push(Cue::flat(Sound::Respawn, 0.7));
        }
        if f.match_start {
            self.streak = 0;
            cues.push(Cue::flat(Sound::MatchStart, 0.9));
        }
        if let Some(win) = f.match_end {
            cues.push(Cue::flat(if win { Sound::Victory } else { Sound::Defeat }, 1.0));
            duck = duck.max(1.0);
        }

        // discs: more of an owner's discs than last frame = a throw; fewer = one ended (ricochet / impact ping).
        // Our own throws already cue from the edge flag, so only other players' discs are positional here.
        if self.seen_first {
            let owners: HashSet<u16> = f.discs.iter().chain(self.prev_discs.iter()).map(|d| d.1).collect();
            for o in owners {
                let now: Vec<_> = f.discs.iter().filter(|d| d.1 == o).collect();
                let was: Vec<_> = self.prev_discs.iter().filter(|d| d.1 == o).collect();
                if now.len() > was.len() && o != f.own_id {
                    if let Some(d) = now.last() {
                        cues.push(Cue::at(Sound::Throw, d.0, 0.7));
                    }
                } else if now.len() < was.len() {
                    if let Some(d) = was.last() {
                        cues.push(Cue::at(Sound::Ricochet, d.0, 0.7));
                    }
                }
            }
        }
        self.prev_discs = f.discs.clone();

        // pieces printed / de-rezzed (skip the first frame: joining a match is not a thousand prints)
        let blocks: HashSet<(u16, [i32; 3])> = f.blocks.iter().copied().collect();
        if self.seen_first {
            let at = |c: [i32; 3]| c.map(|v| (v as f32 + 0.5) * f.block_size);
            for b in blocks.difference(&self.prev_blocks).take(3) {
                cues.push(Cue::at(Sound::Print, at(b.1), 0.6));
            }
            for b in self.prev_blocks.difference(&blocks).take(3) {
                cues.push(Cue::at(Sound::Derez, at(b.1), 0.8));
            }
        }
        self.prev_blocks = blocks;

        // other players: ropes attaching, footsteps, deaths
        let ropes: HashSet<u16> = f.remote_ropes.iter().map(|r| r.0).collect();
        for (id, a) in &f.remote_ropes {
            if !self.prev_ropes.contains(id) {
                cues.push(Cue::at(Sound::GrappleAttach, *a, 0.8));
            }
        }
        self.prev_ropes = ropes;
        let live: HashSet<u16> = f.remote_moving.iter().map(|m| m.0).collect();
        self.remote_steps.retain(|id, _| live.contains(id));
        for (id, pos, speed) in &f.remote_moving {
            match step_interval(*speed) {
                Some(iv) => {
                    let t = self.remote_steps.entry(*id).or_insert(iv * (*id % 5) as f32 / 5.0);
                    *t -= f.dt;
                    if *t <= 0.0 {
                        *t = iv;
                        cues.push(Cue::at(Sound::Step, *pos, 0.5).pitched(0.95 + 0.02 * (*id % 5) as f32));
                    }
                }
                None => {
                    self.remote_steps.remove(id);
                }
            }
        }
        for p in &f.remote_deaths {
            cues.push(Cue::at(Sound::Derez, *p, 0.9));
        }

        self.duck = duck;
        self.seen_first = true;
        (cues, Loops { reel: f.grappling, wall_run: f.wall_running })
    }
}

pub struct AudioFxPlugin;

impl Plugin for AudioFxPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(crate::mixer::load_prefs()).init_resource::<MusicCmds>().init_resource::<ExtraCues>().init_resource::<ExtraMuffle>();
        #[cfg(feature = "audio")]
        backend::install(app);
    }
}

#[cfg(feature = "audio")]
mod backend {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use bevy::audio::{
        AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, Decodable, GlobalVolume, PlaybackSettings, Source, SpatialAudioSink, SpatialListener,
        SpatialScale, Volume,
    };
    use bevy::prelude::*;

    use super::*;
    use crate::mixer::{self, bus_of, crossfade, falloff_range, scan_music, start_next_at, wind_gain, Bus, Ducker, Limiter, Playlist};
    use crate::net::{Net, ViewRes};
    use crate::play::PlayerCam;
    use crate::synth::{render, wav, Sound};

    /// Crossfade length between tracks, seconds.
    const XFADE: f32 = 4.0;
    /// Rodio's own 3D falloff is squeezed so only panning is left; `mixer::falloff` does the distance loudness.
    const SPATIAL: f32 = 0.15;
    /// A one-shot with no sink this long after spawning means there is no audio device.
    const DEVICE_WAIT: f32 = 1.5;
    const MAX_TRACK_BYTES: u64 = 96 * 1024 * 1024;

    #[derive(Resource)]
    struct Bank(HashMap<Sound, Handle<AudioSource>>);

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Device {
        Unknown,
        Ok,
        Missing,
    }

    #[derive(Resource)]
    struct CueState {
        cuer: Cuer,
        limiter: Limiter,
        duck: Ducker,
        muffle: hit_audio::Muffle,
        /// This frame's muffle gain (1 = none) for everything that is not an alert.
        mg: f32,
        device: Device,
        reel: Option<Entity>,
        wall_run: Option<Entity>,
        ambient: bool,
        prev_pos: HashMap<u16, [f32; 3]>,
        prev_menu: bool,
        prev_console: bool,
        saved: AudioSettings,
        save_in: f32,
        booted: bool,
        prev_star_sel: u8,
    }

    impl Default for CueState {
        fn default() -> Self {
            Self {
                cuer: Cuer::default(),
                limiter: Limiter::default(),
                duck: Ducker::default(),
                muffle: hit_audio::Muffle::default(),
                mg: 1.0,
                device: if std::env::var_os("SHINOBI_NOAUDIO").is_some() { Device::Missing } else { Device::Unknown },
                reel: None,
                wall_run: None,
                ambient: false,
                prev_pos: HashMap::new(),
                prev_menu: false,
                prev_console: false,
                saved: AudioSettings::default(),
                save_in: 0.0,
                booted: false,
                prev_star_sel: 0,
            }
        }
    }

    /// A one-shot we spawned; the age lets us detect a missing device and reap strays.
    #[derive(Component)]
    struct Born(f32);

    /// Looping beds: volume is set every frame from the buses.
    #[derive(Component, Clone, Copy, PartialEq, Eq)]
    enum LoopKind {
        Reel,
        WallRun,
        Wind,
        Hum,
    }

    #[derive(Component)]
    struct MusicTrack {
        age: f32,
        duration: Option<f32>,
        /// Seconds since this track started fading out (None = it is the current one).
        fading: Option<f32>,
    }

    #[derive(Resource)]
    struct Music {
        playlist: Playlist,
        current: Option<Entity>,
        announced: bool,
    }

    pub fn install(app: &mut App) {
        let dirs = mixer::music_dirs(
            std::env::var("SHINOBI_MUSIC_DIR").ok().as_deref(),
            std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).as_deref(),
            std::env::current_dir().ok().as_deref(),
        );
        let tracks = scan_music(&dirs);
        if tracks.is_empty() {
            info!("music: no tracks (drop .wav/.ogg/.mp3 files into music/ or set SHINOBI_MUSIC_DIR; see docs/AUDIO.md)");
        } else {
            info!("music: {} tracks from {}", tracks.len(), tracks[0].parent().map(|p| p.display().to_string()).unwrap_or_default());
        }
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
        app.insert_resource(Music { playlist: Playlist::new(tracks, seed), current: None, announced: false })
            .init_resource::<CueState>()
            .add_systems(Startup, build_bank)
            .add_systems(Update, (listener, play, music, reap).chain());
    }

    fn build_bank(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let bank = Sound::ALL.iter().map(|s| (*s, sources.add(AudioSource { bytes: wav(&render(*s)).into() }))).collect();
        commands.insert_resource(Bank(bank));
    }

    /// Ears on the camera (once), for positional sounds.
    fn listener(mut commands: Commands, cam: Query<Entity, (With<PlayerCam>, Without<SpatialListener>)>) {
        for e in &cam {
            commands.entity(e).insert(SpatialListener::new(0.4));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn play(
        mut commands: Commands,
        time: Res<Time>,
        view: Res<ViewRes>,
        net: Res<Net>,
        bank: Option<Res<Bank>>,
        mut settings: ResMut<AudioSettings>,
        mut global: ResMut<GlobalVolume>,
        mut st: ResMut<CueState>,
        menu: Option<Res<crate::hud_view::MenuRes>>,
        mode: Option<Res<crate::play::ModeRes>>,
        mut loops: Query<(&LoopKind, Option<&mut AudioSink>)>,
        mut extra: ResMut<ExtraCues>,
        mut extra_muffle: ResMut<ExtraMuffle>,
    ) {
        let Some(bank) = bank else { return };
        let dt = time.delta_secs();
        global.volume = Volume::Linear(settings.gain());
        let v = &view.0;
        let mut frame = Frame::from_view(v, net.0.own_id().unwrap_or(0), dt);
        let me = frame.own_id;
        frame.star_switched = st.prev_star_sel != v.star_sel as u8;
        st.prev_star_sel = v.star_sel as u8;

        // other players: ropes, walking speed, deaths
        let mut now_pos = HashMap::new();
        for (id, smp) in &v.remotes {
            let id16 = *id as u16;
            if let Some(a) = net.0.game.net.rope_anchor(*id) {
                frame.remote_ropes.push((id16, a));
            }
            if let Some(prev) = st.prev_pos.get(&id16) {
                let d = [smp.pos[0] - prev[0], smp.pos[2] - prev[2]];
                let speed = (d[0] * d[0] + d[1] * d[1]).sqrt() / dt.max(1e-3);
                // stealth cloak (cloak.rs): a cloaked player's footsteps are a tell only while loud (sprinting /
                // dashing); a slow walk or crouch stays as quiet as the shimmer looks
                let cloaked = net.0.game.net.is_cloaked(*id);
                let quiet = cloaked && !shinobi_sim::cloak::loud(speed, net.0.game.cfg.sim.run_speed, false, false);
                if speed < 40.0 && (smp.pos[1] - prev[1]).abs() < 0.05 && !quiet {
                    frame.remote_moving.push((id16, smp.pos, speed));
                }
            }
            now_pos.insert(id16, smp.pos);
        }
        for e in &v.events {
            if let NetEvent::Kill { victim, .. } = e {
                if *victim as u16 != me {
                    if let Some(p) = st.prev_pos.get(&(*victim as u16)).or_else(|| now_pos.get(&(*victim as u16))) {
                        frame.remote_deaths.push(*p);
                    }
                }
            }
        }
        st.prev_pos = now_pos;

        // UI clicks: menu / console opening and closing
        let menu_open = menu.map_or(false, |m| m.menu != crate::hud_view::Menu::Closed);
        let console_open = mode.map_or(false, |m| m.mode() == shinobi_client::input_mode::Mode::Console);
        frame.ui_click = (menu_open && !st.prev_menu) || (console_open && !st.prev_console);
        frame.ui_back = (!menu_open && st.prev_menu) || (!console_open && st.prev_console);
        st.prev_menu = menu_open;
        st.prev_console = console_open;

        // enemy projectiles only: allies' stars do not whoosh
        let my_team = net.0.game.net.my_team();
        frame.threats.retain(|(_, o)| my_team.is_none() || net.0.game.net.team_of(*o) != my_team);
        let (mut cues, lp) = st.cuer.update(&frame);
        cues.append(&mut extra.0);
        if let Some((secs, i)) = st.cuer.muffle {
            st.muffle.hit(secs, i);
        }
        if let Some((secs, i)) = extra_muffle.0.take() {
            st.muffle.hit(secs, i);
        }
        st.mg = st.muffle.update(dt);
        let mg = st.mg;
        // alerts first, so a flurry spends the loudness budget on them before footsteps
        cues.sort_by_key(|c| !hit_audio::is_alert(c.sound));
        let duck = st.cuer.duck;
        st.duck.hit(duck);
        st.limiter.tick(dt);
        let ears = Vec3::from(v.eye);

        if !st.booted {
            st.booted = true;
            commands.spawn((AudioPlayer(bank.0[&Sound::Boot].clone()), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(0.4 * settings.bus(Bus::Sfx))), Born(0.0)));
        }
        if st.device != Device::Missing {
            for c in &cues {
                let Some(h) = bank.0.get(&c.sound) else { continue };
                let mut vol = c.vol;
                if let Some(p) = c.pos {
                    // our own sounds are never attenuated; other players' are by distance
                    vol *= falloff_range(ears.distance(Vec3::from(p)), c.range);
                    if vol < 0.02 {
                        continue;
                    }
                }
                let Some(scale) = st.limiter.admit(c.sound, vol) else { continue };
                let dip = if hit_audio::is_alert(c.sound) { 1.0 } else { mg };
                let vol = vol * scale * dip * settings.bus(bus_of(c.sound));
                let mut e = commands.spawn((
                    AudioPlayer(h.clone()),
                    PlaybackSettings::DESPAWN
                        .with_volume(Volume::Linear(vol))
                        .with_speed(c.speed)
                        .with_spatial(c.pos.is_some())
                        .with_spatial_scale(SpatialScale::new(SPATIAL)),
                    Born(0.0),
                ));
                if let Some(p) = c.pos {
                    e.insert(Transform::from_translation(Vec3::from(p)));
                }
            }
        }

        // loops: want-state spawn / despawn, then every frame's volume from the buses
        let height = v.eye[1];
        let beds = [
            (lp.reel, 0, Sound::Reel, LoopKind::Reel),
            (lp.wall_run, 1, Sound::WallRun, LoopKind::WallRun),
        ];
        for (want, slot, sound, kind) in beds {
            let live = st.device != Device::Missing;
            let cur = if slot == 0 { &mut st.reel } else { &mut st.wall_run };
            match (want && live, *cur) {
                (true, None) => {
                    if let Some(h) = bank.0.get(&sound) {
                        *cur = Some(commands.spawn((AudioPlayer(h.clone()), PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)), kind)).id());
                    }
                }
                (false, Some(e)) => {
                    commands.entity(e).despawn();
                    *cur = None;
                }
                _ => {}
            }
        }
        if !st.ambient && st.device != Device::Missing {
            st.ambient = true;
            for (s, k) in [(Sound::Wind, LoopKind::Wind), (Sound::Hum, LoopKind::Hum)] {
                commands.spawn((AudioPlayer(bank.0[&s].clone()), PlaybackSettings::LOOP.with_volume(Volume::Linear(0.0)), k));
            }
        }
        for (kind, sink) in &mut loops {
            let Some(mut sink) = sink else { continue };
            let g = match kind {
                LoopKind::Reel => 0.5 * mg * settings.bus(Bus::Sfx),
                LoopKind::WallRun => 0.35 * mg * settings.bus(Bus::Sfx),
                LoopKind::Wind => 0.5 * mg * wind_gain(height) * settings.bus(Bus::Ambient),
                LoopKind::Hum => 0.25 * mg * settings.bus(Bus::Ambient),
            };
            let cur = sink.volume().to_linear();
            sink.set_volume(Volume::Linear(cur + (g - cur) * (dt * 8.0).min(1.0)));
        }

        // persist bus changes (debounced)
        if *settings != st.saved {
            st.save_in += dt;
            if st.save_in > 1.0 {
                mixer::save_prefs(&settings);
                st.saved = *settings;
                st.save_in = 0.0;
            }
        } else {
            st.save_in = 0.0;
        }
        let _ = &mut settings;
    }

    /// Detect a missing audio device (a one-shot that never got a sink) and reap strays so nothing piles up.
    fn reap(
        mut commands: Commands,
        time: Res<Time>,
        mut st: ResMut<CueState>,
        mut q: Query<(Entity, &mut Born, Option<&AudioSink>, Option<&SpatialAudioSink>)>,
    ) {
        for (e, mut b, a, s) in &mut q {
            b.0 += time.delta_secs();
            if a.is_some() || s.is_some() {
                if st.device == Device::Unknown {
                    st.device = Device::Ok;
                }
            } else if b.0 > DEVICE_WAIT && st.device == Device::Unknown {
                warn!("audio: no output device; running silent");
                st.device = Device::Missing;
            }
            if b.0 > 6.0 {
                commands.entity(e).despawn();
            }
        }
    }

    /// Validate a track before bevy sees it (bevy unwraps a failing decoder on the audio path), and measure it.
    fn load_track(path: &PathBuf) -> Result<(AudioSource, Option<f32>), String> {
        let md = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if md.len() > MAX_TRACK_BYTES {
            return Err("file too large".into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let src = AudioSource { bytes: bytes.into() };
        let probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| src.decoder().total_duration().map(|d| d.as_secs_f32())));
        match probe {
            Ok(d) => Ok((src, d)),
            Err(_) => Err("cannot decode (only .wav decodes in this build; see docs/AUDIO.md for ogg/mp3)".into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn music(
        mut commands: Commands,
        time: Res<Time>,
        mut sources: ResMut<Assets<AudioSource>>,
        mut m: ResMut<Music>,
        settings: Res<AudioSettings>,
        mut cmds: ResMut<MusicCmds>,
        mut st: ResMut<CueState>,
        mut tracks: Query<(Entity, &mut MusicTrack, Option<&mut AudioSink>)>,
    ) {
        let dt = time.delta_secs();
        let duck = st.duck.update(dt) * st.mg;
        let want = settings.music_on && st.device == Device::Ok && !m.playlist.is_empty();
        let mut start = false;
        if !want {
            for (e, _, _) in &tracks {
                commands.entity(e).despawn();
            }
            m.current = None;
            cmds.next = false;
            return;
        }
        // the current track: finished (despawned by bevy), near its end, or skipped
        match m.current.and_then(|e| tracks.get(e).ok().map(|(_, t, _)| (t.age, t.duration))) {
            None => start = true,
            Some((age, dur)) => {
                if start_next_at(dur, XFADE).map_or(false, |at| age >= at) || cmds.next {
                    start = true;
                }
            }
        }
        cmds.next = false;
        if start {
            // up to a few tries so one broken file does not stall the slot
            for _ in 0..4 {
                let Some(path) = m.playlist.next().map(|p| p.to_path_buf()) else { break };
                match load_track(&path) {
                    Ok((src, dur)) => {
                        info!("music: now playing {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                        if let Some(old) = m.current {
                            if let Ok((_, mut t, _)) = tracks.get_mut(old) {
                                t.fading = Some(0.0);
                            }
                        }
                        let h = sources.add(src);
                        let e = commands
                            .spawn((AudioPlayer(h), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(0.0)), MusicTrack { age: 0.0, duration: dur, fading: None }))
                            .id();
                        m.current = Some(e);
                        break;
                    }
                    Err(why) => {
                        warn!("music: skipping {}: {why}", path.display());
                        m.playlist.remove(&path);
                    }
                }
            }
        }
        let bus = settings.bus(Bus::Music);
        for (e, mut t, sink) in &mut tracks {
            t.age += dt;
            let mut g = bus * duck;
            if let Some(f) = t.fading.as_mut() {
                *f += dt;
                if *f >= XFADE {
                    commands.entity(e).despawn();
                    continue;
                }
                g *= crossfade(*f, XFADE).0;
            } else if t.age < XFADE {
                g *= crossfade(t.age, XFADE).1;
            }
            if let Some(mut s) = sink {
                s.set_volume(Volume::Linear(g));
            }
        }
        let _ = &mut m.announced;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame { dt: 1.0 / 60.0, own_id: 1, block_size: 2.0, ..Default::default() }
    }

    fn sounds(c: &[Cue]) -> Vec<Sound> {
        c.iter().map(|c| c.sound).collect()
    }

    #[test]
    fn only_the_lock_snap_makes_a_sound_and_it_is_a_click() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.lock_edges = vec![LockEdge::Acquired, LockEdge::Locked, LockEdge::Broken];
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::LockClick], "no acquire boop, no break fade");
    }

    #[test]
    fn hits_pick_thud_tick_or_chime() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.dealt = vec![(20, false, false), (HEAVY_HIT, false, false), (30, true, false), (12, false, true), (20, true, true)];
        assert_eq!(
            sounds(&c.update(&f).0),
            vec![Sound::HitBody, Sound::HitHard, Sound::Kill, Sound::HitHard, Sound::Kill, Sound::HitHard],
            "a headshot hits hard; a headshot kill adds the tick on top of the chime"
        );
        assert!(c.duck >= 0.5, "a kill ducks the music");
    }

    #[test]
    fn katana_connect_is_positional_and_whiff_is_quiet() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.slash = true;
        f.slash_hit = Some([1.0, 2.0, 3.0]);
        let cues = c.update(&f).0;
        assert!(cues.iter().any(|q| q.sound == Sound::Clash && q.pos == Some([1.0, 2.0, 3.0])));
        let mut f = frame();
        f.slash_miss = true;
        let w = c.update(&f).0;
        assert_eq!(w.len(), 1);
        assert!(w[0].vol < 0.6);
    }

    #[test]
    fn each_katana_move_has_its_own_sound() {
        let mut seen = HashSet::new();
        for k in [SlashKind::Light, SlashKind::Heavy, SlashKind::Air, SlashKind::Whirl, SlashKind::Counter] {
            let mut f = frame();
            f.slash = true;
            f.slash_kind = k;
            let s: Vec<_> = Cuer::default().update(&f).0.iter().map(|c| (c.sound, c.speed.to_bits())).collect();
            assert!(seen.insert(s.clone()), "{k:?} sounds like another move: {s:?}");
        }
        let mut f = frame();
        f.slash = true;
        f.slash_kind = SlashKind::Air;
        f.slash_hit = Some([0.0; 3]);
        f.slash_killed = true;
        let s = sounds(&Cuer::default().update(&f).0);
        assert!(s.contains(&Sound::AirSlam) && s.contains(&Sound::Execute));
    }

    #[test]
    fn heavy_charge_starts_once_on_the_rising_edge() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.slice_charge = 0.2;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::HeavyCharge]);
        f.slice_charge = 0.6;
        assert!(c.update(&f).0.is_empty());
    }

    #[test]
    fn dash_cues_on_the_rising_edge_only() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.dashing = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Dash]);
        assert!(c.update(&f).0.is_empty(), "held dash is silent");
        f.dashing = false;
        c.update(&f);
        f.dashing = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Dash]);
    }

    #[test]
    fn loops_follow_state() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.grappling = true;
        assert_eq!(c.update(&f).1, Loops { reel: true, wall_run: false });
        f.grappling = false;
        f.wall_running = true;
        assert_eq!(c.update(&f).1, Loops { reel: false, wall_run: true });
    }

    #[test]
    fn rope_pulses_while_reeling() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.grappling = true;
        let mut pulses = 0;
        for _ in 0..180 {
            pulses += c.update(&f).0.iter().filter(|q| q.sound == Sound::RopePulse).count();
        }
        assert!((5..=7).contains(&pulses), "about every half second for 3 s, got {pulses}");
    }

    #[test]
    fn heartbeat_repeats_only_at_low_health() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.hp = Some(0.9);
        assert!(c.update(&f).0.is_empty());
        f.hp = Some(0.2);
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Heartbeat]);
        assert!(c.update(&f).0.is_empty(), "not again until the period passes");
        for _ in 0..60 {
            c.update(&f);
        }
        let mut beats = 0;
        for _ in 0..120 {
            beats += c.update(&f).0.len();
        }
        assert!(beats >= 1, "keeps beating");
    }

    #[test]
    fn other_players_throws_and_ricochets_are_positional_own_throw_is_not() {
        let mut c = Cuer::default();
        let mut f = frame();
        c.update(&f); // first frame primes the diff
        f.discs = vec![([5.0, 1.0, 0.0], 9)];
        let t = c.update(&f).0;
        assert_eq!(t, vec![Cue::at(Sound::Throw, [5.0, 1.0, 0.0], 0.7)]);
        f.discs.clear();
        let r = c.update(&f).0;
        assert_eq!(sounds(&r), vec![Sound::Ricochet]);
        // our own disc: no positional throw (the edge flag cues it)
        f.discs = vec![([0.0; 3], 1)];
        assert!(c.update(&f).0.is_empty());
    }

    #[test]
    fn pieces_print_and_derez_but_joining_is_silent() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.blocks = (0..50).map(|i| (2, [i, 0, 0])).collect();
        assert!(c.update(&f).0.is_empty(), "first frame: existing pieces are not new");
        f.blocks.push((3, [10, 1, 0]));
        let p = c.update(&f).0;
        assert_eq!(p, vec![Cue::at(Sound::Print, [21.0, 3.0, 1.0], 0.6)]);
        f.blocks.pop();
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Derez]);
    }

    #[test]
    fn grapple_sequence_and_remote_rope() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.grapple_attached = Some([1.0, 2.0, 3.0]);
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::GrappleFire, Sound::GrappleAttach]);
        let mut f = frame();
        f.grapple_released = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::GrappleRelease]);
        let mut f = frame();
        f.remote_ropes = vec![(7, [0.0, 5.0, 0.0])];
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::GrappleAttach]);
        assert!(c.update(&f).0.is_empty(), "already attached");
    }

    #[test]
    fn star_lockout_buzzes_and_refill_pings_once() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.star = Some((1.0, 3.0, false));
        assert!(c.update(&f).0.is_empty());
        f.star = Some((0.0, 3.0, true));
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Lockout]);
        assert!(c.update(&f).0.is_empty(), "held lockout is silent");
        f.star_refused = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Lockout], "a refused throw buzzes");
        f.star_refused = false;
        f.star = Some((1.0, 3.0, false));
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Refill]);
        f.star = Some((3.0, 3.0, false));
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Refill], "full pool pings");
        assert!(c.update(&f).0.is_empty());
    }

    #[test]
    fn block_and_parry_are_distinct_and_low_guard_beeps() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.absorbed = 1;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Clang]);
        let mut f = frame();
        f.parried = 1;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Parry, Sound::Parry], "bell plus its bright octave");
        let mut c = Cuer::default();
        let mut f = frame();
        f.blocking = true;
        let mut beeps = 0;
        for _ in 0..240 {
            beeps += c.update(&f).0.iter().filter(|q| q.sound == Sound::GuardLow).count();
        }
        assert!(beeps >= 2, "holding the shield too long beeps ({beeps})");
        f.blocking = false;
        for _ in 0..240 {
            c.update(&f);
        }
        f.blocking = true;
        assert!(c.update(&f).0.is_empty(), "a rested shield is silent");
    }

    #[test]
    fn taking_damage_plays_one_layered_cue_scaled_by_the_worst_hit() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.taken = vec![8];
        let l = c.update(&f).0;
        assert_eq!(sounds(&l), vec![Sound::Hurt]);
        assert!(c.muffle.is_some(), "the world dips");
        let mut f = frame();
        f.taken = vec![10, 34, 12];
        let h = c.update(&f).0;
        assert_eq!(sounds(&h), vec![Sound::HurtHeavy], "one cue for the frame, from the worst hit (>= 30 hp)");
        assert!(h[0].vol > l[0].vol && h[0].speed < l[0].speed);
        let (ms, _) = c.muffle.unwrap();
        assert!(ms > 0.25 && c.duck >= 0.4);
        let mut f = frame();
        f.taken = vec![12];
        f.taken_head = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::HurtHead], "a headshot is the harshest variant");
        assert_eq!(c.update(&frame()).0.len(), 0);
        assert!(c.muffle.is_none());
    }

    #[test]
    fn low_hp_heartbeat_speeds_up_and_stops_on_respawn() {
        let beats = |hp: f32| {
            let mut c = Cuer::default();
            let mut f = frame();
            f.hp = Some(hp);
            (0..360).map(|_| c.update(&f).0.iter().filter(|q| q.sound == Sound::Heartbeat).count()).sum::<usize>()
        };
        assert_eq!(beats(0.5), 0);
        assert!(beats(0.05) > beats(0.28), "{} vs {}", beats(0.05), beats(0.28));
        let mut c = Cuer::default();
        let mut f = frame();
        f.hp = Some(0.1);
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Heartbeat]);
        f.hp = Some(1.0);
        f.respawned = true;
        assert!(c.update(&f).0.iter().all(|q| q.sound != Sound::Heartbeat), "no beat after respawning");
        f.respawned = false;
        for _ in 0..200 {
            assert!(c.update(&f).0.is_empty());
        }
    }

    #[test]
    fn enemy_projectile_passing_close_whooshes_once_far_one_does_not() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.eye = [0.0, 1.6, 0.0];
        f.threats = vec![([12.0, 1.6, 1.0], 7)];
        assert!(c.update(&f).0.is_empty());
        f.threats = vec![([6.0, 1.6, 1.0], 7)];
        c.update(&f);
        f.threats = vec![([1.0, 1.6, 1.0], 7)];
        let w = c.update(&f).0;
        assert_eq!(sounds(&w), vec![Sound::Whoosh]);
        assert!(w[0].pos.is_some(), "positional");
        f.threats = vec![([-5.0, 1.6, 1.0], 7)];
        assert!(c.update(&f).0.is_empty(), "once per pass");
        let mut c = Cuer::default();
        for x in [30.0, 20.0, 10.0, 0.0] {
            f.threats = vec![([x, 1.6, 9.0], 7)];
            assert!(c.update(&f).0.is_empty(), "9 m to the side is not a near miss");
        }
    }

    #[test]
    fn guard_breaks_after_enough_shield_hits_and_arrow_on_shield_is_not_doubled() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.blocking = true;
        f.absorbed = 1;
        let mut broke = 0;
        let mut clangs = 0;
        for _ in 0..6 {
            let s = sounds(&c.update(&f).0);
            broke += s.iter().filter(|x| **x == Sound::GuardBreak).count();
            clangs += s.iter().filter(|x| **x == Sound::Clang).count();
        }
        assert_eq!((broke, clangs), (1, 6), "every hit clangs, the broken guard breaks once");
        let mut f = frame();
        f.eye = [0.0, 1.6, 0.0];
        f.absorbed = 1;
        f.power_fx = vec![(shinobi_sim::power_pass::FX_ARROW_BLOCKED, [0.0, 1.6, 1.0], 9)];
        assert_eq!(sounds(&Cuer::default().update(&f).0), vec![Sound::Clang], "the world copy of our own block is skipped");
        f.power_fx = vec![(shinobi_sim::power_pass::FX_ARROW_BLOCKED, [40.0, 1.6, 1.0], 9)];
        assert_eq!(sounds(&Cuer::default().update(&f).0), vec![Sound::Block, Sound::Clang], "someone else's block is still heard");
    }

    #[test]
    fn jump_land_and_double_jump() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.jumped = true;
        f.double_jumped = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::DoubleJump, Sound::Jump].into_iter().rev().collect::<Vec<_>>());
        let mut f = frame();
        f.landed = true;
        f.landing_speed = 5.0;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Land]);
        f.landing_speed = 15.0;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::LandHard]);
        f.landing_speed = 1.0;
        assert!(c.update(&f).0.is_empty(), "a hop is silent");
    }

    #[test]
    fn footsteps_follow_speed_and_stop_in_the_air() {
        let count = |speed: f32, grounded: bool| {
            let mut c = Cuer::default();
            let mut f = frame();
            f.grounded = grounded;
            f.speed = speed;
            (0..120).map(|_| c.update(&f).0.iter().filter(|q| q.sound == Sound::Step).count()).sum::<usize>()
        };
        assert_eq!(count(0.5, true), 0);
        assert_eq!(count(8.0, false), 0);
        assert!(count(9.0, true) > count(3.0, true));
        assert!(count(3.0, true) >= 3);
    }

    #[test]
    fn remote_players_step_positionally() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.remote_moving = vec![(4, [10.0, 1.0, 0.0], 8.0)];
        let steps: usize = (0..120).map(|_| c.update(&f).0.iter().filter(|q| q.sound == Sound::Step && q.pos == Some([10.0, 1.0, 0.0])).count()).sum();
        assert!(steps >= 3);
        f.remote_moving.clear();
        assert!(c.update(&f).0.is_empty());
        f.remote_deaths = vec![[1.0, 1.0, 1.0]];
        assert_eq!(c.update(&f).0, vec![Cue::at(Sound::Derez, [1.0, 1.0, 1.0], 0.9)]);
    }

    #[test]
    fn kill_streak_rises_in_pitch_and_resets_on_death() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.my_kills = 1;
        assert!(c.update(&f).0.is_empty(), "first kill: the kill chime comes from the hit");
        let a = c.update(&f).0;
        let b = c.update(&f).0;
        assert_eq!(sounds(&a), vec![Sound::Streak]);
        assert!(b[0].speed > a[0].speed, "rising");
        let mut d = frame();
        d.my_death = Some(3);
        c.update(&d);
        assert!(c.update(&f).0.is_empty(), "streak reset by dying");
    }

    #[test]
    fn flow_tier_up_stings_once_per_rise_not_on_fall_or_hold() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.flow_tier = 1;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::FlowTierUp], "0 -> 1 is a rise");
        assert!(c.update(&f).0.is_empty(), "holding the same tier stings nothing more");
        f.flow_tier = 3;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::FlowTierUp], "1 -> 3 is still one rise");
        f.flow_tier = 1;
        assert!(c.update(&f).0.is_empty(), "falling back down is silent");
        f.flow_tier = 2;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::FlowTierUp], "climbing again rises");
    }

    #[test]
    fn stealth_kill_gets_its_own_sting_on_top_of_the_plain_kill_chime() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.dealt = vec![(90, true, false)];
        f.stealth_kill = true;
        let s = sounds(&c.update(&f).0);
        assert_eq!(s, vec![Sound::Kill, Sound::StealthKill]);
    }

    #[test]
    fn bounty_kill_stings_when_the_victim_was_the_live_bounty_as_of_last_frame_and_becoming_the_bounty_stings_once() {
        let mut c = Cuer::default();
        let own = frame().own_id; // 1
        // the local player becomes the LIVE bounty: a rising stinger, once
        let mut f = frame();
        f.bounty_now = Some((own, true));
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::BecomeBounty]);
        assert!(c.update(&f).0.is_empty(), "holding the title stings nothing more");
        // somebody else (id 7) is marked live; we kill them this frame -> bounty kill sting
        let mut g = frame();
        g.bounty_now = Some((7, true));
        c.update(&g); // records bounty_seen = (7, true) for next frame's judgement
        let mut k = frame();
        k.my_kill_victims = vec![7];
        assert_eq!(sounds(&c.update(&k).0), vec![Sound::BountyKill]);
        // killing someone who was NOT the live bounty is silent on this front
        let mut k2 = frame();
        k2.my_kill_victims = vec![42];
        assert!(sounds(&c.update(&k2).0).iter().all(|s| *s != Sound::BountyKill));
    }

    #[test]
    fn lifecycle_stingers() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.my_death = Some(0);
        let s = sounds(&c.update(&f).0);
        assert_eq!(s, vec![Sound::VoidFall, Sound::DeathSting], "void death whooshes then stings");
        assert!(c.duck >= 0.8);
        let mut f = frame();
        f.my_death = Some(5);
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::DeathSting]);
        let mut f = frame();
        f.respawned = true;
        f.match_start = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Respawn, Sound::MatchStart]);
        let mut f = frame();
        f.match_end = Some(true);
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Victory]);
        f.match_end = Some(false);
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Defeat]);
    }

    #[test]
    fn ui_clicks() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.ui_click = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::UiClick]);
        let mut f = frame();
        f.ui_back = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::UiBack]);
    }

    #[test]
    fn settings_gain_respects_mute_and_clamps() {
        assert_eq!(AudioSettings { volume: 0.5, muted: true, ..Default::default() }.gain(), 0.0);
        assert_eq!(AudioSettings { volume: 3.0, ..Default::default() }.gain(), 1.0);
        assert_eq!(AudioSettings::default().gain(), 0.7);
    }

    #[test]
    fn frame_from_an_empty_view_is_quiet() {
        let f = Frame::from_view(&RenderView::default(), 1, 0.016);
        let mut c = Cuer::default();
        let (cues, loops) = c.update(&f);
        assert!(cues.is_empty() && loops == Loops::default());
    }

    #[test]
    fn every_cue_sound_exists_in_the_bank() {
        // a cue can only play if the synth renders it: all Sound variants are in ALL
        let all: HashSet<_> = Sound::ALL.iter().collect();
        for s in [
            Sound::Parry, Sound::Block, Sound::Lockout, Sound::Refill, Sound::Streak, Sound::Wind, Sound::Hum, Sound::UiClick,
            Sound::EarRing, Sound::FlashBang, Sound::Tinnitus, Sound::WhipCrack,
        ] {
            assert!(all.contains(&s));
        }
    }

    #[test]
    fn dazed_muffle_lasts_the_whole_daze() {
        assert_eq!(dazed_muffle(1.0, 3.5), (3.5, 1.0));
        assert!(dazed_muffle(0.2, 3.5).1 < dazed_muffle(1.0, 3.5).1);
    }

    #[test]
    fn a_flash_burst_cracks_where_it_goes_off() {
        use shinobi_sim::events::STAR_FX_FLASH_BURST;
        let mut c = Cuer::default();
        let mut f = frame();
        f.star_fx = vec![(STAR_FX_FLASH_BURST, [10.0, 1.0, 0.0])];
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::FlashBang]);
    }

    #[test]
    fn being_dazed_whines_and_mutes_for_the_daze() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.blind = Some((1.0, 3.5));
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::Tinnitus]);
        f.blind = None;
        c.update(&f);
        assert_eq!(c.muffle, Some((3.5, 1.0)));
    }

    #[test]
    fn the_hook_cracks_once_on_catching_an_enemy() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.pulling_enemy = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::WhipCrack]);
        assert!(c.update(&f).0.is_empty(), "once per catch");
        f.pulling_enemy = false;
        c.update(&f);
        f.pulling_enemy = true;
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::WhipCrack]);
    }
}
