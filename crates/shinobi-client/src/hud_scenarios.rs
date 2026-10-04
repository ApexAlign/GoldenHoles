//! Fixed HUD scenarios (spawn, fight with lock, console, scoreboard, radial menu) as draw lists. Shared by the
//! golden layout test and the `hud_dump` example that renders them to images.

use crate::console::{self, Console};
use crate::hud::{
    callsign, AllyTag, BuildHud, BuildSlot, GrappleHud, Hud, HudState, KillEntry, KilledBy, Palette, Prim, RadarPing, ScoreRow, TargetHud,
    Telemetry, Winner,
};
use crate::radial;
use shinobi_sim::combat::DamageKind;
use crate::colour::player_rgba;

fn score(id: u16, kills: u16, deaths: u16, local: bool) -> ScoreRow {
    ScoreRow { name: callsign(id), kills, deaths, ping_ms: 20 + id * 7, local, alive: id % 5 != 3, team: None, colour: Some(player_rgba(id)) }
}

thread_local! {
    static ASPECT: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

fn base() -> HudState {
    HudState {
        aspect: ASPECT.with(|a| a.get()),
        hp: 0.8,
        dash_ready: 1.0,
        shuriken_ready: 0.6,
        slice_ready: 1.0,
        spread: 0.2,
        ffa: true,
        kill_limit: Some(10),
        time_limit_secs: Some(360),
        heading: Some(0.5),
        zone: Some(("HUB SPIRE".into(), [0.2, 0.8, 1.0])),
        telemetry: Some(Telemetry { speed: 13.4, altitude: 4.2, rope: None }),
        grapple: Some(GrappleHud { ready: 1.0, hookable: false, on_rope: false, missed: false }),
        build: Some(BuildHud { selected: BuildSlot::Floor, ready: 1.0, placed: 6, max: 64, ghost: Some(true) }),
        ping_ms: Some(34),
        match_clock: Some((shinobi_sim::match_state::PhaseKind::Live, Some(412))),
        scoreboard: vec![score(1, 9, 3, false), score(2, 7, 4, true), score(3, 5, 5, false), score(4, 4, 6, false), score(5, 2, 7, false), score(6, 1, 8, false)],
        time: 2.0,
        ..Default::default()
    }
}

fn kill(k: u16, v: u16, w: DamageKind, age: f32) -> KillEntry {
    KillEntry { killer: callsign(k), victim: callsign(v), weapon: w, age, involves_local: k == 2 || v == 2, void: false, killer_team: None, victim_team: None, killer_colour: Some(player_rgba(k)), victim_colour: Some(player_rgba(v)) }
}

/// `(name, draw list)` for each scenario.
pub fn scenarios() -> Vec<(&'static str, Vec<Prim>)> {
    scenarios_at(0.0)
}

/// The same scenarios laid out for a screen of width / height `aspect` (0 = the 16:9 the goldens use).
pub fn scenarios_at(aspect: f32) -> Vec<(&'static str, Vec<Prim>)> {
    ASPECT.with(|a| a.set(aspect));
    let v = scenarios_raw();
    ASPECT.with(|a| a.set(0.0));
    v.into_iter().map(|(n, p)| (n, crate::hud::with_text_shadows(p, &Palette::TRON))).collect()
}

fn scenarios_raw() -> Vec<(&'static str, Vec<Prim>)> {
    let p = Palette::TRON;
    let mut out = Vec::new();

    let spawn = HudState { boot: Some(1.1), hp: 1.0, controls: crate::controls_card::ControlsCard::default().rows(), ..base() };
    out.push(("spawn", Hud::build_with(&spawn, &p)));

    let fight = HudState {
        hp: 0.28,
        since_hit: Some(0.08),
        hit_kind: Some(DamageKind::Shuriken),
        hit_amount: 34,
        since_damage: Some((0.25, 1.1)),
        damage_frac: 0.35,
        target: Some(TargetHud { pos: (0.58, 0.42), name: callsign(1), colour: player_rgba(1), hp: Some(0.6), dist: 18.0, progress: 1.0, locked: true }),
        radar_pickups: vec![(-0.35, 0.3), (0.45, -0.2)],
        radar_pieces: vec![(0.1, 0.25), (0.15, 0.3), (-0.2, 0.1)],
        radar_edge: (0..14).map(|i| ((0.9 - i as f32 * 0.02, -0.4 - i as f32 * 0.03), (0.88 - i as f32 * 0.02, -0.43 - i as f32 * 0.03))).collect(),
        radar: vec![
            RadarPing { pos: (0.3, 0.5), colour: player_rgba(1), ally: false },
            RadarPing { pos: (-0.5, 0.2), colour: player_rgba(3), ally: false },
        ],
        killfeed: vec![kill(2, 3, DamageKind::Shuriken, 0.4), kill(1, 4, DamageKind::Slash, 1.8), kill(5, 6, DamageKind::GridBeam, 3.0)],
        build: Some(BuildHud { selected: BuildSlot::Ramp, ready: 0.4, placed: 12, max: 64, ghost: Some(false) }),
        grapple: Some(GrappleHud { ready: 0.7, hookable: true, on_rope: false, missed: false }),
        ally_tags: vec![AllyTag { pos: (0.35, 0.5), name: callsign(8), dist: 22.0, clamped: false }],
        wall_run: true,
        ..base()
    };
    out.push(("fight_lock", Hud::build_with(&fight, &p)));

    // hit from behind-left while two enemies lurk off screen
    let flank = HudState {
        since_damage: Some((0.2, -2.4)),
        damage_frac: 0.3,
        enemy_tags: vec![
            crate::hud::EnemyTag { pos: (0.42, 0.45), name: callsign(2), dist: 8.0, team: None, colour: Some(player_rgba(2)), hp: None },
            crate::hud::EnemyTag { pos: (0.64, 0.5), name: callsign(3), dist: 21.0, team: None, colour: Some(player_rgba(3)), hp: Some(0.6) },
        ],
        enemy_ticks: vec![crate::hud::EnemyTick { bearing: 1.9, dist: 12.0 }, crate::hud::EnemyTick { bearing: -0.4 - 1.0, dist: 33.0 }],
        ..base()
    };
    out.push(("flank", Hud::build_with(&flank, &p)));

    let dead = HudState {
        dead: true,
        hp: 0.0,
        respawn_in_s: Some(1.8),
        killed_by: Some(KilledBy { name: callsign(1), colour: Some(player_rgba(1)), weapon: DamageKind::Shuriken, void: false, bearing: Some(-0.9), dist: Some(31.0), kd: Some((7, 4)) }),
        audio: Some((0.7, false)),
        ..base()
    };
    out.push(("death", Hud::build_with(&dead, &p)));

    let results = HudState {
        show_scoreboard: true,
        match_clock: Some((shinobi_sim::match_state::PhaseKind::Intermission, Some(6))),
        winner: Some(Winner { name: callsign(1), colour: Some(player_rgba(1)), local: false, draw: false }),
        awards: vec![
            crate::hud::Award { title: "SHARPSHOOTER", name: callsign(1), colour: Some(player_rgba(1)), value: "6".into() },
            crate::hud::Award { title: "BLADEMASTER", name: callsign(3), colour: Some(player_rgba(3)), value: "4".into() },
            crate::hud::Award { title: "LONGEST KILL", name: callsign(2), colour: Some(player_rgba(2)), value: "52 M".into() },
        ],
        my_headshots: 3,
        audio: Some((0.7, true)),
        ..base()
    };
    out.push(("results", Hud::build_with(&results, &p)));

    let waiting = HudState { match_clock: Some((shinobi_sim::match_state::PhaseKind::Warmup, None)), shield: Some(0.7), scoreboard: vec![], ..base() };
    out.push(("waiting", Hud::build_with(&waiting, &p)));

    // damage direction (bearing: 0 ahead, + clockwise) and hit markers, each on a quiet HUD
    let quiet = || HudState { hp: 0.7, boot: None, scoreboard: vec![], killfeed: vec![], ..base() };
    for (name, bearing, frac) in [("hit_taken_left", -1.5f32, 0.3f32), ("hit_taken_right", 1.5, 0.6), ("hit_taken_behind", 3.1, 0.2)] {
        let s = HudState { since_damage: Some((0.15, bearing)), damage_frac: frac, ..quiet() };
        out.push((name, Hud::build_with(&s, &p)));
    }
    for (name, kind, kill, amount) in [("hit_dealt_shuriken", DamageKind::Shuriken, false, 12u8), ("hit_dealt_slash", DamageKind::Slash, false, 60), ("hit_dealt_kill", DamageKind::Shuriken, true, 40), ("hit_dealt_head", DamageKind::Shuriken, false, 18)] {
        let s = HudState { since_hit: Some(0.06), hit_kind: Some(kind), hit_kill: kill, hit_amount: amount, hit_head: name == "hit_dealt_head", ..quiet() };
        out.push((name, Hud::build_with(&s, &p)));
    }

    out.push(("settings", crate::screens::draw_settings(&crate::settings::Settings::default(), 0, &p)));

    let mut con = Console::default();
    let mut st = crate::settings::Settings::default();
    for line in ["/help", "/fov 100", "/colour", "/log"] {
        for ch in line.chars() {
            con.type_char(ch);
        }
        con.submit(&mut st, &[], 2);
    }
    con.input = "/sens 0.003".into();
    let mut prims = Hud::build_with(&base(), &p);
    prims.extend(console::draw(&con, 0.0, &p));
    out.push(("console", prims));

    let board = HudState { show_scoreboard: true, cursor: Some((0.5, 0.435)), ..base() };
    out.push(("scoreboard", Hud::build_with(&board, &p)));

    let mut prims = Hud::build_with(&base(), &p);
    prims.extend(radial::draw_with(Some(BuildSlot::Ramp), BuildSlot::Floor, &p, if ASPECT.with(|a| a.get()) > 0.1 { ASPECT.with(|a| a.get()) } else { 16.0 / 9.0 }));
    out.push(("radial", prims));
    out
}

/// One line per primitive, 3 decimals: stable text for golden files.
pub fn dump(prims: &[Prim]) -> String {
    let f = |v: f32| format!("{v:.3}");
    let c = |c: &crate::hud::Rgba| format!("{},{},{},{}", f(c.0), f(c.1), f(c.2), f(c.3));
    let mut s = String::new();
    for p in prims {
        let line = match p {
            Prim::Rect { r, color, glow } => format!("rect {} {} {} {} {} g{}", f(r.x), f(r.y), f(r.w), f(r.h), c(color), f(*glow)),
            Prim::Line { a, b, color, glow } => format!("line {} {} {} {} {} g{}", f(a.0), f(a.1), f(b.0), f(b.1), c(color), f(*glow)),
            Prim::Vignette { start, max_a, tint } => format!("vignette {} {} {}", f(*start), f(*max_a), c(tint)),
            Prim::Scanlines { count, scroll, a } => format!("scanlines {count} {} {}", f(*scroll), f(*a)),
            Prim::Ring { c: ce, r, color, fill, glow } => format!("ring {} {} {} {} fill={fill} g{}", f(ce.0), f(ce.1), f(*r), c(color), f(*glow)),
            Prim::Text { pos, text, size, color, align } => format!("text {} {} {} {:?} {:?} {}", f(pos.0), f(pos.1), f(*size), text, align, c(color)),
            Prim::Grid { r, cell, color } => format!("grid {} {} {} {} {} {}", f(r.x), f(r.y), f(r.w), f(r.h), f(*cell), c(color)),
        };
        s.push_str(&line);
        s.push('\n');
    }
    s
}
