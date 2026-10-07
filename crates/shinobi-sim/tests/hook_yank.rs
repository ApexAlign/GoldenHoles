//! Hook yank and rope slash (docs/traversal_melee.md), server side: the hook key again on a player rope pulls that player to
//! you and staggers them; a katana swing on a taut rope cuts enemies near the rope line.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

const FRAME: Duration = Duration::from_nanos(16_666_667);

fn balance() -> Balance {
    let mut b = Balance::demo();
    b.combat.katana.damage = 20.0;
    b.melee = shinobi_sim::balance::MeleeBalance::default();
    b
}

fn send(s: &mut Server, id: u16, it: PlayerIntent) {
    let t = s.tick;
    s.ingest(id, &ClientPacket { intents: vec![PlayerIntent { tick: t, ..it }], ..Default::default() }.encode().unwrap());
}

fn put(s: &mut Server, id: u16, at: [f32; 3]) {
    let i = s.players.iter().position(|p| p.id == id).unwrap();
    let b = s.players[i].ch.body;
    s.world.bodies[b].set_position(Isometry::translation(at[0], at[1], at[2]), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, b);
}

fn pos(s: &Server, id: u16) -> [f32; 3] {
    s.players.iter().find(|p| p.id == id).unwrap().pos(&s.world)
}

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// A at the origin looking down -z, a back wall at z = -80, players 1..=3.
fn scene() -> Server {
    let mut s = Server::new(Cfg::default());
    s.set_balance(balance()).unwrap();
    s.add_static_box([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]);
    s.add_static_box([0.0, 20.0, -81.0], [60.0, 20.0, 1.0]);
    s.add_player(1, [0.0, 0.81, 0.0]);
    s.add_player(2, [0.0, 0.81, -30.0]);
    s.add_player(3, [0.0, 0.81, -30.0]);
    for _ in 0..400 {
        s.frame(FRAME);
    }
    for p in s.players.iter_mut() {
        p.vitals.invuln_until_tick = 0;
    }
    s
}

fn idle() -> PlayerIntent {
    PlayerIntent::default()
}

fn zip() -> PlayerIntent {
    PlayerIntent { zip: true, ..Default::default() }
}

fn step(s: &mut Server, intents: &[(u16, PlayerIntent)]) {
    for (id, it) in intents {
        send(s, *id, *it);
    }
    s.frame(FRAME);
}

#[test]
fn hook_key_again_on_a_player_rope_pulls_them_in_and_staggers_them() {
    let mut s = scene();
    put(&mut s, 3, [150.0, 0.81, 150.0]);
    put(&mut s, 1, [0.0, 0.81, 0.0]);
    put(&mut s, 2, [0.0, 0.81, -30.0]);
    step(&mut s, &[]); // the hitboxes follow the bodies
    step(&mut s, &[(1, zip())]);
    assert!(s.players[0].ch.grapple.is_some(), "hooked");
    // a few ticks of rope (the reel starts), then the hook key again
    for _ in 0..3 {
        step(&mut s, &[(1, idle())]);
    }
    let gap0 = dist(pos(&s, 1), pos(&s, 2));
    let vic0 = pos(&s, 2);
    step(&mut s, &[(1, zip())]);
    assert!(s.players[0].ch.grapple.is_none(), "the yank cuts the rope");
    let stag = s.tick;
    assert!(s.players[1].blk.staggered(stag), "victim staggered");
    let (mut best, mut t_best) = (f32::MAX, 0u32);
    let mut at_1s = 0.0;
    for k in 0..90u32 {
        step(&mut s, &[(1, idle())]);
        let d = dist(pos(&s, 1), pos(&s, 2));
        if d < best {
            best = d;
            t_best = k;
        }
        if k == 59 {
            at_1s = d;
        }
    }
    let moved = dist(vic0, pos(&s, 2));
    eprintln!("hook yank: gap {gap0:.1} m -> closest {best:.1} m at {:.2} s, {at_1s:.1} m after 1 s (victim moved {moved:.1} m)", t_best as f32 / 60.0);
    assert!(best < 6.0 && best > 0.5, "the target arrives within 6 m of the yanker: {best}");
    assert!(t_best < 60, "inside a second: {t_best}");
    // cooldown: the hook cannot fire again for yank_cd
    step(&mut s, &[(1, zip())]);
    assert!(s.players[0].ch.grapple.is_none(), "yank cooldown holds the next hook ({:.2} s)", s.players[0].ch.grapple_cooldown());
}

#[test]
fn a_rope_slash_cuts_an_enemy_on_the_rope_line_and_misses_one_off_it() {
    let mut s = scene();
    let k = balance().combat.katana;
    // aim at the back wall; 2 sits 1 m off the rope line (outside the hitbox ray, inside the rope radius), 3 sits 5 m off it
    put(&mut s, 1, [0.0, 0.81, 0.0]);
    put(&mut s, 2, [1.0, 0.81, -14.0]);
    put(&mut s, 3, [5.0, 0.81, -14.0]);
    step(&mut s, &[]);
    step(&mut s, &[(1, zip())]);
    assert!(s.players[0].ch.grapple.is_some(), "hooked the wall");
    let a = s.players[0].ch.grapple.unwrap();
    assert!((a.anchor.z + 80.0).abs() < 1.5, "geometry anchor, not a player: {:?}", a.anchor);
    let (h2, h3) = (hp(&s, 2), hp(&s, 3));
    step(&mut s, &[(1, PlayerIntent { buttons: Buttons(Buttons::SLICE), ..Default::default() })]);
    for _ in 0..3 {
        step(&mut s, &[(1, idle())]);
    }
    let (d2, d3) = (h2 - hp(&s, 2), h3 - hp(&s, 3));
    let expect = k.damage * k.rope_damage_mult;
    eprintln!("rope slash: on-line enemy took {d2:.1} (katana {} x rope {} = {expect:.1}), off-line enemy took {d3:.1}", k.damage, k.rope_damage_mult);
    assert!((d2 - expect).abs() < 1.0, "on the line: {d2} vs {expect}");
    assert_eq!(d3, 0.0, "off the line: untouched");
}
