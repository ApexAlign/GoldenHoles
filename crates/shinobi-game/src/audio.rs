//! Game audio. Two layers:
//!   * `Cuer` (always compiled, pure, unit-tested): turns per-frame game state (`Frame`, built from `RenderView`) into
//!     sound cues and loop on/off states. No sound card, no bevy audio.
//!   * the playback backend (cargo feature `audio`): renders `synth` sounds to WAV at startup, plays cues with
//!     `bevy_audio`, positions other players' sounds in 3D and applies `AudioSettings`. Without the feature the game
//!     builds and runs silent (the dev box has no alsa headers). Run with sound: `cargo run --features audio`.

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::collections::HashSet;

use bevy::prelude::*;

use shinobi_client::client_game::RenderView;
use shinobi_sim::combat::aim_assist::LockEdge;

use crate::synth::Sound;

/// Master volume and mute (the HUD console's `/vol` and `/mute` write this).
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct AudioSettings {
    /// 0..=1 linear.
    pub volume: f32,
    pub muted: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { volume: 0.7, muted: false }
    }
}

impl AudioSettings {
    /// Linear gain actually applied (0 when muted).
    pub fn gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume.clamp(0.0, 1.0) }
    }
}

/// One sound to play: `pos` = world position for 3D placement (None = plain, centred, our own).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub sound: Sound,
    pub pos: Option<[f32; 3]>,
    pub vol: f32,
}

impl Cue {
    fn at(sound: Sound, pos: [f32; 3], vol: f32) -> Self {
        Self { sound, pos: Some(pos), vol }
    }
    fn flat(sound: Sound, vol: f32) -> Self {
        Self { sound, pos: None, vol }
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
    pub slash_hit: Option<[f32; 3]>,
    pub slash_killed: bool,
    pub slash_miss: bool,
    pub grapple_attached: Option<[f32; 3]>,
    pub grapple_released: bool,
    pub grapple_missed: bool,
    pub grappling: bool,
    pub dashing: bool,
    pub wall_running: bool,
    /// Health 0..=1 (None before the first extras block).
    pub hp: Option<f32>,
    /// Hits we dealt: (damage, killed, headshot).
    pub dealt: Vec<(u8, bool, bool)>,
    /// Damage amounts we took.
    pub taken: Vec<u8>,
    /// Discs in flight: (position, owner id).
    pub discs: Vec<([f32; 3], u16)>,
    /// Pieces: (owner, cell) and the cell edge in metres.
    pub blocks: Vec<(u16, [i32; 3])>,
    pub block_size: f32,
    /// Other players' ropes that are attached: (player, anchor).
    pub remote_ropes: Vec<(u16, [f32; 3])>,
}

impl Frame {
    pub fn from_view(v: &RenderView, own_id: u16, dt: f32) -> Frame {
        let f = &v.fx;
        Frame {
            dt,
            own_id,
            lock_edges: v.lock_edges.clone(),
            threw: f.threw.is_some(),
            slash: f.slash.is_some(),
            slash_hit: f.slash_hit.map(|h| h.point),
            slash_killed: f.slash_hit.map_or(false, |h| h.killed),
            slash_miss: f.slash_miss,
            grapple_attached: f.grapple_attached,
            grapple_released: f.grapple_released,
            grapple_missed: f.grapple_missed,
            grappling: f.grappling,
            dashing: f.dashing,
            wall_running: f.wall_running,
            hp: v.hp.map(|h| (h / 100.0).clamp(0.0, 1.0)),
            dealt: v.hits.iter().filter(|h| !h.taken).map(|h| (h.amount, h.kill, h.head)).collect(),
            taken: v.hits.iter().filter(|h| h.taken).map(|h| h.amount).collect(),
            discs: v.discs.iter().enumerate().map(|(i, (p, _))| (*p, v.disc_owners.get(i).copied().unwrap_or(0))).collect(),
            blocks: v.blocks.iter().map(|b| (b.owner, b.cell)).collect(),
            block_size: v.block_size,
            remote_ropes: Vec::new(),
        }
    }
}

/// Loops that should be playing right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loops {
    pub reel: bool,
    pub wall_run: bool,
}

/// Damage at or above which a dealt hit uses the sharp "heavy hit" tick instead of the body thud.
pub const HEAVY_HIT: u8 = 60;
const LOW_HP: f32 = 0.3;
const HEARTBEAT_PERIOD: f32 = 0.9;

/// Edge detector over frames: remembers last frame's discs / pieces / ropes and emits a `Cue` for each change.
#[derive(Default)]
pub struct Cuer {
    prev_dashing: bool,
    prev_discs: Vec<([f32; 3], u16)>,
    prev_blocks: HashSet<(u16, [i32; 3])>,
    prev_ropes: HashSet<u16>,
    seen_first: bool,
    heart: f32,
}

impl Cuer {
    pub fn update(&mut self, f: &Frame) -> (Vec<Cue>, Loops) {
        let mut cues = Vec::new();

        for e in &f.lock_edges {
            cues.push(Cue::flat(
                match e {
                    LockEdge::Acquired => Sound::LockTone,
                    LockEdge::Locked => Sound::LockChirp,
                    LockEdge::Broken => Sound::LockBreak,
                },
                0.8,
            ));
        }
        if f.threw {
            cues.push(Cue::flat(Sound::Throw, 0.8));
        }
        if f.slash {
            cues.push(Cue::flat(Sound::Swing, 0.8));
        }
        if let Some(p) = f.slash_hit {
            cues.push(Cue::at(Sound::Clash, p, 1.0));
        }
        if f.slash_miss {
            cues.push(Cue::flat(Sound::Whiff, 0.5));
        }
        if f.grapple_missed {
            cues.push(Cue::flat(Sound::GrappleFire, 0.4));
        } else if f.grapple_attached.is_some() {
            cues.push(Cue::flat(Sound::GrappleFire, 0.7));
        }
        if let Some(p) = f.grapple_attached {
            cues.push(Cue::at(Sound::GrappleAttach, p, 1.0));
        }
        if f.grapple_released {
            cues.push(Cue::flat(Sound::GrappleRelease, 0.7));
        }
        if f.dashing && !self.prev_dashing {
            cues.push(Cue::flat(Sound::Dash, 0.7));
        }
        self.prev_dashing = f.dashing;

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
            // a headshot kill also gets the sharp precision tick on top of the kill chime
            if *head && *kill {
                cues.push(Cue::flat(Sound::HitHard, 1.0));
            }
        }
        for amount in &f.taken {
            cues.push(Cue::flat(Sound::Hurt, (0.5 + *amount as f32 / 100.0).min(1.0)));
        }

        // low-health heartbeat
        match f.hp {
            Some(h) if h > 0.0 && h < LOW_HP => {
                self.heart -= f.dt;
                if self.heart <= 0.0 {
                    self.heart = HEARTBEAT_PERIOD;
                    cues.push(Cue::flat(Sound::Heartbeat, 0.9));
                }
            }
            _ => self.heart = 0.0,
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

        // other players' ropes attaching
        let ropes: HashSet<u16> = f.remote_ropes.iter().map(|r| r.0).collect();
        for (id, a) in &f.remote_ropes {
            if !self.prev_ropes.contains(id) {
                cues.push(Cue::at(Sound::GrappleAttach, *a, 0.8));
            }
        }
        self.prev_ropes = ropes;

        self.seen_first = true;
        (cues, Loops { reel: f.grappling, wall_run: f.wall_running })
    }
}

pub struct AudioFxPlugin;

impl Plugin for AudioFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioSettings>();
        #[cfg(feature = "audio")]
        backend::install(app);
    }
}

#[cfg(feature = "audio")]
mod backend {
    use std::collections::HashMap;

    use bevy::audio::{AudioPlayer, AudioSource, GlobalVolume, PlaybackSettings, SpatialListener, Volume};
    use bevy::prelude::*;

    use super::*;
    use crate::net::{Net, ViewRes};
    use crate::play::PlayerCam;
    use crate::synth::{render, wav, Sound};

    #[derive(Resource)]
    struct Bank(HashMap<Sound, Handle<AudioSource>>);

    #[derive(Resource, Default)]
    struct CueState {
        cuer: Cuer,
        reel: Option<Entity>,
        wall_run: Option<Entity>,
    }

    pub fn install(app: &mut App) {
        app.init_resource::<CueState>().add_systems(Startup, build_bank).add_systems(Update, (listener, play));
    }

    fn build_bank(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let bank = Sound::ALL
            .iter()
            .map(|s| (*s, sources.add(AudioSource { bytes: wav(&render(*s)).into() })))
            .collect();
        commands.insert_resource(Bank(bank));
    }

    /// Ears on the camera (once), for positional sounds.
    fn listener(mut commands: Commands, cam: Query<Entity, (With<PlayerCam>, Without<SpatialListener>)>) {
        for e in &cam {
            commands.entity(e).insert(SpatialListener::new(0.4));
        }
    }

    fn play(
        mut commands: Commands,
        time: Res<Time>,
        view: Res<ViewRes>,
        net: Res<Net>,
        bank: Option<Res<Bank>>,
        settings: Res<AudioSettings>,
        mut global: ResMut<GlobalVolume>,
        mut st: ResMut<CueState>,
    ) {
        let Some(bank) = bank else { return };
        global.volume = Volume::Linear(settings.gain());
        let v = &view.0;
        let mut frame = Frame::from_view(v, net.0.own_id().unwrap_or(0), time.delta_secs());
        for (id, smp) in &v.remotes {
            if let Some(a) = net.0.game.net.rope_anchor(*id) {
                let _ = smp;
                frame.remote_ropes.push((*id, a));
            }
        }
        let (cues, loops) = st.cuer.update(&frame);
        let spawn = |commands: &mut Commands, c: &Cue| {
            let Some(h) = bank.0.get(&c.sound) else { return };
            let mut e = commands.spawn((AudioPlayer(h.clone()), PlaybackSettings::DESPAWN.with_volume(Volume::Linear(c.vol)).with_spatial(c.pos.is_some())));
            if let Some(p) = c.pos {
                e.insert(Transform::from_translation(Vec3::from(p)));
            }
        };
        for c in &cues {
            spawn(&mut commands, c);
        }
        for (want, slot, sound, vol) in [(loops.reel, 0, Sound::Reel, 0.5), (loops.wall_run, 1, Sound::WallRun, 0.35)] {
            let cur = if slot == 0 { &mut st.reel } else { &mut st.wall_run };
            match (want, *cur) {
                (true, None) => {
                    if let Some(h) = bank.0.get(&sound) {
                        *cur = Some(commands.spawn((AudioPlayer(h.clone()), PlaybackSettings::LOOP.with_volume(Volume::Linear(vol)))).id());
                    }
                }
                (false, Some(e)) => {
                    commands.entity(e).despawn();
                    *cur = None;
                }
                _ => {}
            }
        }
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
    fn lock_edges_map_to_tone_chirp_and_fade() {
        let mut c = Cuer::default();
        let mut f = frame();
        f.lock_edges = vec![LockEdge::Acquired, LockEdge::Locked, LockEdge::Broken];
        assert_eq!(sounds(&c.update(&f).0), vec![Sound::LockTone, Sound::LockChirp, Sound::LockBreak]);
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
    fn settings_gain_respects_mute_and_clamps() {
        assert_eq!(AudioSettings { volume: 0.5, muted: true }.gain(), 0.0);
        assert_eq!(AudioSettings { volume: 3.0, muted: false }.gain(), 1.0);
        assert_eq!(AudioSettings::default().gain(), 0.7);
    }

    #[test]
    fn frame_from_an_empty_view_is_quiet() {
        let f = Frame::from_view(&RenderView::default(), 1, 0.016);
        let mut c = Cuer::default();
        let (cues, loops) = c.update(&f);
        assert!(cues.is_empty() && loops == Loops::default());
    }
}
