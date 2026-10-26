use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_sim::character::{sync_body_colliders, Cfg, PhysicsWorld};
use shinobi_sim::features::{FeatureEvent, Features, LEVEL_HAZARD_ID_BASE};
use shinobi_sim::level::{parse_level_file, LevelDesc, LevelFeatures, MoveAxis};
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

const ONE_TICK: Duration = Duration::from_nanos(16_666_667);

fn setup(text: &str) -> Server {
    let f = parse_level_file(text).unwrap();
    let mut s = Server::new(Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_level_features(f.features.clone());
    for (i, spawn) in f.level.spawns.iter().enumerate() {
        s.add_player(i as u16 + 1, *spawn);
    }
    s
}

fn step(s: &mut Server) {
    s.frame(ONE_TICK);
}

fn place(s: &mut Server, id: u16, pos: [f32; 3]) {
    let body = s.players.iter().find(|p| p.id == id).unwrap().ch.body;
    s.world.bodies[body].set_position(Isometry::translation(pos[0], pos[1], pos[2]), true);
    sync_body_colliders(&mut s.world, body);
}

fn positions(s: &Server) -> Vec<[f32; 3]> {
    s.players.iter().map(|p| p.pos(&s.world)).collect()
}

#[test]
fn parse_errors_name_the_feature_line() {
    for (text, needle) in [
        ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 0 0 1 1 1\n", "line 3: wrong number"),
        ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 0 0 1 1 1 vent bad\n", "line 3: expected key=value"),
        ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nmover 0 0 0 1 1 1 w 2 10 solid p\n", "line 3: mover axis"),
        ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nmover 0 0 0 1 1 1 x 2 10 maybe p\n", "line 3: mover must be solid or ghost"),
        ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nhazard 0 0 0 1 1 1 0 1 5 h\n", "line 3: period_ticks must be > 0"),
        ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 0 0 1 1 1 vent cooldown_ticks=1.5\n", "line 3: tick tuning"),
    ] {
        let e = parse_level_file(text).unwrap_err();
        assert!(e.contains(needle), "{text:?} -> {e}");
    }
}

#[test]
fn feature_text_roundtrip_is_exact() {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\npickup 8 1 0 5\n\
        zone 2 1 3 2 1 2 vent fill_rate=0.5 cooldown_ticks=90\n\
        mover 6 2 0 1 2 1 x 6 180 solid piston phase_ticks=30\n\
        mover -6 2 0 1 1 1 z 4 120 ghost fan\n\
        hazard 0 4 0 2 1 2 180 36 20 vent_fire active_ticks=12\n";
    let f = parse_level_file(text).unwrap();
    assert_eq!(f.features.zones.len(), 1);
    assert_eq!(f.features.movers.len(), 2);
    assert_eq!(f.features.hazards.len(), 1);
    assert_eq!(parse_level_file(&f.to_text()).unwrap(), f);
    assert!(f.to_text().contains("mover 6 2 0 1 2 1 x 6 180 solid piston phase_ticks=30"));
}

#[test]
fn mover_position_is_an_exact_function_of_tick() {
    let f = parse_level_file(
        "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
         mover 0 4 0 1 1 1 x 6 100 solid piston\n\
         mover 0 4 0 1 1 1 y 4 80 ghost lift phase_ticks=20\n",
    )
    .unwrap();
    let m = &f.features.movers[0];
    assert_eq!(m.axis, MoveAxis::X);
    assert_eq!(m.pos_at(0), [0.0, 4.0, 0.0]);
    assert_eq!(m.pos_at(25), [1.5, 4.0, 0.0]);
    assert_eq!(m.pos_at(50), [3.0, 4.0, 0.0]);
    assert_eq!(m.pos_at(100), [6.0, 4.0, 0.0]);
    assert_eq!(m.pos_at(150), [3.0, 4.0, 0.0]);
    assert_eq!(m.pos_at(200), [0.0, 4.0, 0.0]);
    assert_eq!(m.pos_at(1000), m.pos_at(0), "periodic and exact");
    // phase shifts the same cycle: 20 ticks into an 80-tick leg is a quarter of the 4 m lift
    assert_eq!(f.features.movers[1].pos_at(0), [0.0, 5.0, 0.0]);
}

#[test]
fn zone_enter_and_exit_edges_are_server_authoritative_and_cool_down() {
    let mut s = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
         zone 0 1 0 2 1 2 vent cooldown_ticks=10\n",
    );
    step(&mut s);
    assert_eq!(s.level_features.events(), &[FeatureEvent::ZoneEnter { tick: 0, zone: 0, player: 1 }]);
    step(&mut s);
    assert!(s.level_features.events().is_empty(), "still inside: no repeated edge");

    place(&mut s, 1, [10.0, 0.81, 0.0]);
    step(&mut s);
    assert_eq!(s.level_features.events(), &[FeatureEvent::ZoneExit { tick: 2, zone: 0, player: 1 }]);

    place(&mut s, 1, [0.0, 0.81, 0.0]);
    while s.tick < 12 {
        step(&mut s);
        assert!(s.level_features.events().is_empty(), "re-entry waits out cooldown_ticks");
    }
    step(&mut s);
    assert_eq!(s.level_features.events(), &[FeatureEvent::ZoneEnter { tick: 12, zone: 0, player: 1 }]);
}

#[test]
fn hazard_cadence_is_tick_exact_and_deals_damage_once_per_cycle() {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
        hazard 0 1 0 3 2 3 30 5 10 zap\n";
    let f = parse_level_file(text).unwrap();
    let h = &f.features.hazards[0];
    assert!(h.is_active(0) && h.is_active(30) && h.is_active(60));
    assert!(!h.is_active(1) && !h.is_active(29));
    assert!(h.is_telegraph(25) && h.is_telegraph(29));
    assert!(!h.is_telegraph(24) && !h.is_telegraph(0));

    let mut s = setup(text);
    let max_hp = s.players[0].vitals.max_hp;
    s.players[0].vitals.invuln_until_tick = 0;
    let mut hit_ticks = Vec::new();
    let mut saw_source = false;
    for _ in 0..65 {
        let tick = s.tick;
        step(&mut s);
        if s.level_features.events().iter().any(|e| matches!(e, FeatureEvent::HazardHit { .. })) {
            hit_ticks.push(tick);
            saw_source |= s.pvp_events.iter().any(|e| matches!(
                e,
                shinobi_sim::pvp::PvpEvent::HazardHit { victim: 1, amount, source: shinobi_sim::pvp::HazardSource::Obstacle(id), .. }
                    if *amount == 10.0 && *id == LEVEL_HAZARD_ID_BASE
            ));
        }
    }
    assert_eq!(hit_ticks, vec![0, 30, 60]);
    assert_eq!(s.players[0].vitals.hp, max_hp - 30.0);
    assert!(saw_source, "level hazards report through the ordinary HazardSource::Obstacle path");
}

#[test]
fn solid_mover_blocks_a_runner_and_pushes_a_player_out_of_its_face() {
    let mut block = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
         mover 2 2 0 0.5 2 2 x 0.01 60000 solid wall\n",
    );
    for _ in 0..120 {
        let tick = block.tick;
        let pkt = ClientPacket { intents: vec![PlayerIntent { tick, move_x: 127, ..Default::default() }], ..Default::default() };
        block.ingest(1, &pkt.encode().unwrap());
        step(&mut block);
    }
    let x = block.players[0].pos(&block.world)[0];
    assert!(x > 0.5, "walked toward the wall (x={x})");
    assert!(x < 1.3, "the solid mover blocks the runner (x={x})");

    let mut push = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 4 0.81 0\n\
         mover 2 2 0 0.5 2 2 x 4 60 solid piston\n",
    );
    for _ in 0..30 {
        step(&mut push);
    }
    let x = push.players[0].pos(&push.world)[0];
    assert!(x > 4.5, "piston face displaced the player (x={x})");
    for _ in 30..60 {
        step(&mut push);
    }
    let x = push.players[0].pos(&push.world)[0];
    assert!(x > 6.5, "the player rides the full stroke instead of tunnelling (x={x})");
}

#[test]
fn a_fast_mover_cannot_tunnel_and_does_not_drag_on_the_return_stroke() {
    let mut s = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 4 0.81 0\n\
         mover 2 2 0 0.5 2 2 x 10 2 solid piston\n",
    );
    step(&mut s);
    step(&mut s);
    let pos = s.players[0].pos(&s.world);
    assert!(pos[0] >= 7.0, "a 5 m/tick stroke sweeps the player to the leading face (x={})", pos[0]);
    step(&mut s);
    let pos = s.players[0].pos(&s.world);
    assert!(pos[0] >= 12.0, "still ahead of the mover at the far end (x={})", pos[0]);
    for _ in 0..4 {
        step(&mut s);
    }
    let pos = s.players[0].pos(&s.world);
    assert!(pos[0] > 12.0, "the return stroke leaves the player where it was (x={})", pos[0]);
}

#[test]
fn one_thousand_ticks_replay_identically() {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 4 0.81 0\n\
        zone 4 1 0 3 2 3 lane cooldown_ticks=20\n\
        mover 2 2 0 0.5 2 2 x 4 60 solid piston\n\
        hazard 4 1 0 4 2 3 30 5 1 zap\n";

    fn replay(text: &str) -> Vec<(u32, Vec<[u32; 3]>, u32, [u32; 3], Vec<FeatureEvent>)> {
        let mut s = setup(text);
        let max_hp = s.players[0].vitals.max_hp;
        s.players[0].vitals.invuln_until_tick = 0;
        let mut out = Vec::new();
        for _ in 0..1000 {
            step(&mut s);
            out.push((
                s.tick,
                positions(&s).iter().map(|p| p.map(f32::to_bits)).collect(),
                s.players[0].vitals.hp.to_bits(),
                s.level_features.mover_position(0, s.tick - 1).unwrap().map(f32::to_bits),
                s.level_features.events().to_vec(),
            ));
        }
        assert!(s.players[0].vitals.hp < max_hp, "hazards ran during the replay");
        out
    }

    assert_eq!(replay(text), replay(text));
}

fn client_world(level: &LevelDesc, features: &LevelFeatures, cfg: &Cfg, start: [f32; 3]) -> (PhysicsWorld, Features, RigidBodyHandle) {
    let mut world = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
    level.insert_into(&mut world);
    let mut runtime = Features::default();
    runtime.load(features.clone(), &mut world);
    let body = world
        .bodies
        .insert(RigidBodyBuilder::kinematic_position_based().translation(vector![start[0], start[1], start[2]]).build());
    world.colliders.insert_with_parent(
        ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
            .collision_groups(InteractionGroups::new(shinobi_sim::combat::GROUP_PLAYER, Group::ALL))
            .build(),
        body,
        &mut world.bodies,
    );
    (world, runtime, body)
}

fn assert_push_parity(text: &str) {
    let f = parse_level_file(text).unwrap();
    let cfg = Cfg::default();
    let mut server = Server::new(cfg);
    f.level.apply_to_server(&mut server);
    server.set_level_features(f.features.clone());
    for (i, spawn) in f.level.spawns.iter().enumerate() {
        server.add_player(i as u16 + 1, *spawn);
    }
    let start = server.players[0].pos(&server.world);
    let (mut world, features, body) = client_world(&f.level, &f.features, &cfg, start);

    for tick in 0..120 {
        server.level_features.begin_tick(&mut server.world, &mut server.players, tick, &cfg);
        features.begin_tick_single(&mut world, body, tick, &cfg);
        let server_pos = server.players[0].pos(&server.world);
        let t = world.bodies[body].translation();
        let client_pos = [t.x, t.y, t.z];
        assert_eq!(server_pos.map(f32::to_bits), client_pos.map(f32::to_bits), "tick {tick}");
    }
}

#[test]
fn server_begin_tick_and_client_single_body_push_are_tick_identical() {
    assert_push_parity("box 0 -0.5 0 50 0.5 50\nspawn 4 0.81 0\nmover 2 2 0 0.5 2 2 x 4 60 solid piston\n");
    assert_push_parity("box 0 -0.5 0 50 0.5 50\nspawn 0 3.3 0\nmover 0 2 0 1 0.5 1 y 4 60 solid lift\n");
    // riders: carried on a sliding platform and on a rising lift
    assert_push_parity("box 0 -0.5 0 50 0.5 50\nspawn 0 2.06 0\nmover 0 1 0 2 0.25 2 z 5 90 solid platform\n");
    assert_push_parity("box 0 -0.5 0 50 0.5 50\nspawn 0 1.31 0\nmover 0 0.25 0 2 0.25 2 y 3 120 solid lift\n");
}

#[test]
fn a_player_standing_on_a_sliding_platform_rides_it_and_one_beside_it_does_not() {
    let mut s = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 0 2.06 0\nspawn 0 0.81 -6\n\
         mover 0 1 0 2 0.25 2 x 6 120 solid platform\n",
    );
    for _ in 0..60 {
        step(&mut s);
    }
    let rider = s.players[0].pos(&s.world);
    let platform = s.level_features.mover_position(0, s.tick - 1).unwrap();
    assert!(platform[0] > 2.5, "the platform moved (x={})", platform[0]);
    assert!((rider[0] - platform[0]).abs() < 0.3, "rider kept pace: rider x {} platform x {}", rider[0], platform[0]);
    assert!((rider[1] - 2.06).abs() < 0.15, "rider stayed on top (y={})", rider[1]);
    let bystander = s.players[1].pos(&s.world);
    assert!(bystander[0].abs() < 0.1, "a player on the floor beside it is not carried (x={})", bystander[0]);
}

#[test]
fn a_rising_lift_carries_its_rider_up_and_a_jump_still_leaves_it() {
    let mut s = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 0 1.31 0\n\
         mover 0 0.25 0 2 0.25 2 y 3 120 solid lift\n",
    );
    for _ in 0..60 {
        step(&mut s);
    }
    let y = s.players[0].pos(&s.world)[1];
    let top = s.level_features.mover_position(0, s.tick - 1).unwrap()[1] + 0.25;
    assert!(top > 1.8, "the lift rose (top={top})");
    assert!((y - 0.81 - top).abs() < 0.12, "rider stands on the lift top: feet {} top {top}", y - 0.81);

    let tick = s.tick;
    let pkt = ClientPacket { intents: vec![PlayerIntent { tick, buttons: Buttons(Buttons::JUMP_PRESSED | Buttons::JUMP_HELD), ..Default::default() }], ..Default::default() };
    s.ingest(1, &pkt.encode().unwrap());
    for _ in 0..12 {
        step(&mut s);
    }
    let y = s.players[0].pos(&s.world)[1];
    let top = s.level_features.mover_position(0, s.tick - 1).unwrap()[1] + 0.25;
    assert!(y - 0.81 > top + 0.3, "the jump leaves the lift: feet {} top {top}", y - 0.81);
}


#[test]
fn a_piston_that_shoves_an_unguarded_player_hurts_and_staggers_once_per_stroke() {
    let mut s = setup(
        "box 0 -0.5 0 50 0.5 50\nspawn 4 0.81 0\n\
         mover 2 2 0 0.5 2 2 x 4 60 solid piston damage=15 stagger_ticks=30\n",
    );
    let max_hp = s.players[0].vitals.max_hp;
    s.players[0].vitals.invuln_until_tick = 0;
    let mut strikes = 0;
    let mut first = None;
    for _ in 0..60 {
        let tick = s.tick;
        step(&mut s);
        if !s.level_features.piston_strikes().is_empty() {
            strikes += 1;
            first.get_or_insert(tick);
        }
    }
    let first = first.expect("the piston face reached the player");
    assert_eq!(strikes, 1, "one strike per stroke even while the face keeps shoving");
    assert_eq!(s.players[0].vitals.hp, max_hp - 15.0);
    assert!(s.players[0].blk.stagger_until >= first + 30, "staggered from the strike");
}

#[test]
fn a_parry_against_the_piston_face_launches_along_it_instead() {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 4 0.81 0\n\
         mover 2 2 0 0.5 2 2 x 4 60 solid piston damage=15 parry_reach=12 parry_up=4\n";
    // dry run: when does the face first reach a player who stands still?
    let mut dry = setup(text);
    let mut contact = None;
    for _ in 0..60 {
        let tick = dry.tick;
        step(&mut dry);
        if !dry.level_features.piston_strikes().is_empty() {
            contact = Some(tick);
            break;
        }
    }
    let contact = contact.expect("the piston face reached the player");
    assert!(contact >= 2, "room to raise the shield first");

    let mut s = setup(text);
    let max_hp = s.players[0].vitals.max_hp;
    s.players[0].vitals.invuln_until_tick = 0;
    let mut launched = None;
    for _ in 0..60 {
        let tick = s.tick;
        // raise the shield one tick before the face arrives: inside the parry window
        let block = tick + 1 >= contact;
        let pkt = ClientPacket { intents: vec![PlayerIntent { tick, block, ..Default::default() }], ..Default::default() };
        s.ingest(1, &pkt.encode().unwrap());
        step(&mut s);
        if !s.level_features.piston_strikes().is_empty() {
            launched = Some(s.players[0].ch.vel);
            break;
        }
    }
    let v = launched.expect("the piston face reached the player");
    assert!(v.x > 5.0, "launched along the face normal (+x): {v:?}");
    assert!(v.y > 5.0, "and up: {v:?}");
    assert_eq!(s.players[0].vitals.hp, max_hp, "a parry takes no damage");
    let x0 = s.players[0].pos(&s.world)[0];
    for _ in 0..30 {
        step(&mut s);
    }
    assert!(s.players[0].pos(&s.world)[0] > x0 + 4.0, "the launch carries the player well clear");
}

#[test]
fn a_glitch_trap_rides_its_piston_and_only_hurts_where_the_face_is_now() {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 6 0.81 0\n\
        mover 0 2 0 0.5 2 2 x 6 120 ghost track\n\
        hazard 0 2 0 0.6 2 2 10 0 5 glitch mover=0\n";
    let f = parse_level_file(text).unwrap();
    let h = &f.features.hazards[0];
    assert_eq!(h.center_at(0, &f.features.movers), [0.0, 2.0, 0.0]);
    assert_eq!(h.center_at(60, &f.features.movers), [3.0, 2.0, 0.0], "carried by the mover's displacement");

    let mut s = setup(text);
    let max_hp = s.players[0].vitals.max_hp;
    s.players[0].vitals.invuln_until_tick = 0;
    for _ in 0..80 {
        step(&mut s);
    }
    assert_eq!(s.players[0].vitals.hp, max_hp, "the trap has not reached the player yet");
    for _ in 80..130 {
        step(&mut s);
    }
    assert!(s.players[0].vitals.hp < max_hp, "the trap rode the piston onto the player");

    let err = parse_level_file("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nhazard 0 1 0 1 1 1 10 0 5 glitch mover=3\n").unwrap_err();
    assert!(err.contains("rides mover 3"), "{err}");
}

#[test]
fn tube_and_ring_lines_parse_round_trip_and_build_closed_segment_loops() {
    use shinobi_sim::level::ShapeKind;
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
        tube 10 5 0 4 10 0.5 24 glass\n\
        ring 0 8 0 24 2.5 0.5 48 ring_mid\n";
    let f = parse_level_file(text).unwrap();
    assert_eq!(f.features.shapes.len(), 2);
    assert_eq!(parse_level_file(&f.to_text()).unwrap(), f, "exact round trip");

    let tube = &f.features.shapes[0];
    assert_eq!(tube.kind, ShapeKind::Tube);
    let segs = tube.segments();
    assert_eq!(segs.len(), 24);
    for s in &segs {
        let (dx, dz) = (s.center[0] - 10.0, s.center[2]);
        let r = (dx * dx + dz * dz).sqrt();
        assert!((r - s.half[0] - 4.0).abs() < 1e-4, "inner face sits on the radius: {}", r - s.half[0]);
        assert!((s.half[1] - 5.0).abs() < 1e-6, "full wall height");
    }
    // neighbouring segments close the loop: the chord covers the gap at the outer edge
    let step = 2.0 * std::f32::consts::PI / 24.0;
    assert!(2.0 * segs[0].half[2] >= 2.0 * 4.5 * (step / 2.0).sin() - 1e-4);

    let ring = &f.features.shapes[1];
    assert_eq!(ring.segments().len(), 48);
    assert!(ring.segments().iter().all(|s| (s.center[1] - 8.0).abs() < 1e-6 && (s.half[0] - 1.25).abs() < 1e-6));

    for (bad, needle) in [
        ("tube 0 0 0 0 10 0.5 24 t\n", "must be > 0"),
        ("tube 0 0 0 4 10 0.5 2 t\n", "segments"),
        ("ring 0 0 0 2 5 0.5 24 r\n", "ring width"),
    ] {
        let e = parse_level_file(&format!("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n{bad}")).unwrap_err();
        assert!(e.contains(needle), "{bad}: {e}");
    }
}

#[test]
fn shapes_do_not_change_the_fingerprint_of_files_without_them_but_do_change_it_when_added() {
    let base = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 1 0 2 1 2 vent\n";
    let a = parse_level_file(base).unwrap().features.fingerprint();
    let b = parse_level_file(&format!("{base}tube 10 5 0 4 10 0.5 24 glass\n")).unwrap().features.fingerprint();
    assert_ne!(a, b);
}

#[test]
fn a_player_stands_on_a_ring_and_a_tube_wall_stops_a_runner() {
    let mut s = setup(
        "box 0 -0.5 0 60 0.5 60\nspawn 24 9.06 0\nspawn 0 0.81 0\n\
         ring 0 8 0 24 2.5 0.5 48 ring\n\
         tube 0 5 0 4 10 0.5 24 glass\n",
    );
    for _ in 0..60 {
        let tick = s.tick;
        let pkt = ClientPacket { intents: vec![PlayerIntent { tick, move_x: 127, ..Default::default() }], ..Default::default() };
        s.ingest(2, &pkt.encode().unwrap());
        step(&mut s);
    }
    let on_ring = s.players[0].pos(&s.world);
    assert!((on_ring[1] - 9.06).abs() < 0.15, "standing on the ring top (y={})", on_ring[1]);
    let runner = s.players[1].pos(&s.world);
    let r = (runner[0] * runner[0] + runner[2] * runner[2]).sqrt();
    assert!(r < 4.0, "the runner inside the tube is stopped by its wall (r={r})");
}
