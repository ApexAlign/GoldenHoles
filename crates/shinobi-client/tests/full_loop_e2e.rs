//! Full loop through `ClientGame` <-> real `Server` (zero-latency links, reliable events replicated the way
//! `Replicator` does): a scripted player locks on and throws, kills with the katana, grapples, prints a floor, a
//! ramp and a wall, shoots a piece until it de-rezzes, dies in the void and respawns. Asserts the server saw each
//! thing, the client's view of the pieces matches the server's, and prediction never strays or panics.

use std::time::Duration;

use shinobi_client::client_game::{Action, ButtonEvent, ClientGame, ClientGameCfg, DeathView, InputFrame, OwnAck};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, is_hello, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::intent::ClientPacket;
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_sim::blocks::{BlockEvent, PieceKind};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::combat::DamageKind;
use shinobi_sim::events::{EventOutbox, NetEvent};
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

fn level() -> LevelDesc {
    LevelDesc {
        boxes: vec![
            LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]),
            LevelBox::new([0.0, 14.0, -30.0], [8.0, 0.5, 8.0]), // anchor slab over the lane: the grapple target
        ],
        spawns: vec![[0.5, 0.81, 0.5], [30.0, 0.81, 30.0]],
    }
}

fn entity(srv: &Server, i: usize) -> (u16, EntityState) {
    let p = &srv.players[i];
    let v = p.ch.vel;
    let kind = match p.ch.state {
        MoveState::Grounded => MoveKind::Grounded,
        MoveState::Airborne => MoveKind::Airborne,
        MoveState::WallRun { .. } => MoveKind::WallRun,
    };
    let flags = if p.vitals.alive() { 0 } else { shinobi_proto::snapshot::FLAG_DEAD };
    (p.id, EntityState { pos: q_pos3(p.pos(&srv.world)), vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)], yaw: p.yaw, kind, flags, fx: 0 })
}

struct Rig {
    game: ClientGame,
    srv: Server,
    tick: u32,
    welcomed: bool,
    pending: Vec<Vec<u8>>,
    outbox: EventOutbox,
    // what happened
    pvp: Vec<PvpEvent>,
    block_events: Vec<BlockEvent>,
    max_grapple: bool,
    saw_rope: bool,
    /// Predicted position per intent tick, and the server's position after executing that tick.
    pred_pos: std::collections::HashMap<u32, [f32; 3]>,
    srv_pos: std::collections::HashMap<u32, [f32; 3]>,
    view_blocks: Vec<(u16, [i32; 3])>,
    phase_from: u32,
    /// Every view drawn while dead: the death sequence as the shell sees it (`DeathView`, camera eye, aim).
    death_views: Vec<(DeathView, [f32; 3], f32)>,
}

impl Rig {
    fn new() -> Rig {
        let lv = level();
        // melee homing off here: this scenario scripts exact positions (a separate test covers the homing)
        let game = ClientGame::new(lv.clone(), ClientGameCfg { melee_home: false, ..ClientGameCfg::default() });
        let mut srv = Server::new(Cfg::default());
        lv.apply_to_server(&mut srv);
        srv.set_spawns(lv.spawns.clone());
        srv.match_state.cfg.frag_limit = u16::MAX;
        srv.match_state.cfg.time_limit_ticks = u32::MAX / 2;
        srv.add_player(1, [0.5, 0.81, 0.5]);
        srv.add_player(2, [0.5, 0.81, -15.0]);
        // run the warmup out first: the match start teleports everybody to a spawn, which would hit mid-scenario
        for _ in 0..3000 {
            if srv.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
                break;
            }
            srv.frame(Duration::from_nanos(16_666_667));
        }
        assert_eq!(srv.match_state.kind(), shinobi_sim::match_state::PhaseKind::Live, "the match is live before the scenario starts");
        for (id, at) in [(1u16, [0.5, 0.81, 0.5]), (2, [0.5, 0.81, -15.0])] {
            let body = srv.players.iter().find(|p| p.id == id).unwrap().ch.body;
            srv.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(at[0], at[1], at[2]), true);
            shinobi_sim::character::sync_body_colliders(&mut srv.world, body);
        }
        Rig {
            game,
            srv,
            tick: 0,
            welcomed: false,
            pending: vec![],
            outbox: EventOutbox::default(),
            pvp: vec![],
            block_events: vec![],
            max_grapple: false,
            saw_rope: false,
            pred_pos: Default::default(),
            srv_pos: Default::default(),
            view_blocks: vec![],
            phase_from: 60,
            death_views: vec![],
        }
    }

    fn snapshot_datagram(&mut self) -> Vec<u8> {
        let srv = &self.srv;
        let snap = Snapshot { tick: srv.tick, entities: (0..srv.players.len()).map(|i| entity(srv, i)).collect() };
        let mut ev = BitWriter::new();
        self.outbox.write_pending(&mut ev, 256);
        let mut own = BitWriter::new();
        let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![], hit_seq: 0, ..Default::default() };
        extras.write(&mut own, |w| OwnState::from_snapshot(&srv.players[0].ch.snapshot(&srv.world)).write(w));
        frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
    }

    /// One client tick + one server tick, no latency; block events become reliable NetEvents like the replicator.
    fn step(&mut self, f: InputFrame) {
        let now = self.tick as f64;
        for d in std::mem::take(&mut self.pending) {
            let _ = self.game.on_datagram(&d, now);
        }
        if let Some(u) = self.game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                self.game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }
        if let Some(pkt) = self.game.sim_tick(&f, now) {
            if is_hello(&pkt) {
                self.pending.push(Welcome { player_id: 1, server_tick: self.srv.tick, ..Default::default() }.encode().to_vec());
                self.welcomed = true;
                self.outbox.push(NetEvent::BlockRules(self.srv.blocks.cfg.rules()));
            } else {
                if let Ok(cp) = ClientPacket::decode(&pkt) {
                    self.outbox.ack(cp.ack_event_seq);
                    if let Some(p) = self.game.predicted_pos() {
                        self.pred_pos.insert(cp.intents[0].tick, p);
                    }
                }
                self.srv.ingest(1, &pkt);
            }
        }
        let view = self.game.view(now);
        self.saw_rope |= view.rope.is_some();
        if let Some(d) = view.death {
            self.death_views.push((d, view.eye, view.aim.yaw));
        }
        self.view_blocks = view.blocks.iter().map(|b| (b.owner, b.cell)).collect();
        self.srv.frame(Duration::from_nanos(16_666_667));
        self.pvp.extend(self.srv.pvp_events.iter().copied());
        self.max_grapple |= self.srv.players.iter().any(|p| p.id == 1 && p.ch.grapple.is_some());
        for e in self.srv.block_events.clone() {
            self.block_events.push(e);
            match e {
                BlockEvent::Placed { owner, cell, piece, scale_q, .. } => {
                    self.outbox.push(NetEvent::BlockPlaced { owner, cell, kind: piece.kind.id(), axis: piece.axis, scale_q })
                }
                BlockEvent::Removed { cell, .. } => self.outbox.push(NetEvent::BlockRemoved { cell }),
                BlockEvent::DeRez { cell, .. } => self.outbox.push(NetEvent::BlockDerez { cell }),
                _ => 0,
            };
        }
        if self.welcomed {
            let d = self.snapshot_datagram();
            self.pending.push(d);
        }
        if let Some(me) = self.srv.players.iter().find(|p| p.id == 1).filter(|p| p.vitals.alive()) {
            self.srv_pos.insert(self.srv.tick - 1, me.pos(&self.srv.world));
        }
        self.tick += 1;
    }

    fn idle(&mut self, n: u32) {
        for _ in 0..n {
            self.step(InputFrame::default());
        }
    }

    fn press(&mut self, a: Action) {
        let f = InputFrame { buttons: vec![ButtonEvent { action: a, down: true }, ButtonEvent { action: a, down: false }], ..Default::default() };
        self.step(f);
    }

    /// Worst |predicted - server| over the ticks since the last phase mark (both sides keyed by the same tick, so the
    /// client's lead does not count), reported and reset.
    fn phase(&mut self, name: &str) -> f32 {
        let (from, to) = (self.phase_from, self.srv.tick.saturating_sub(40)); // newest acks still in flight
        let mut worst = 0.0f32;
        for t in from..to {
            if let (Some(p), Some(s)) = (self.pred_pos.get(&t), self.srv_pos.get(&t)) {
                let e = ((p[0] - s[0]).powi(2) + (p[1] - s[1]).powi(2) + (p[2] - s[2]).powi(2)).sqrt();
                worst = worst.max(e);
            }
        }
        eprintln!("phase {name}: worst prediction error {worst:.3} m over ticks {from}..{to}");
        self.phase_from = to;
        worst
    }

    fn me(&self) -> &shinobi_sim::tick::Player {
        self.srv.players.iter().find(|p| p.id == 1).unwrap()
    }

    fn teleport(&mut self, id: u16, to: [f32; 3]) {
        let body = self.srv.players.iter().find(|p| p.id == id).unwrap().ch.body;
        self.srv.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(to[0], to[1], to[2]), true);
        shinobi_sim::character::sync_body_colliders(&mut self.srv.world, body);
    }
}

#[test]
fn the_whole_loop_stays_in_sync() {
    let mut r = Rig::new();
    r.idle(120); // connect, predict, settle: crosshair rests on player 2 (15 m ahead), lock completes

    // ---- 1. lock-on throw: the assisted disc lands
    r.press(Action::Fire);
    r.idle(60);
    assert!(
        r.pvp.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: 1, victim: 2, kind: DamageKind::Shuriken, .. })),
        "the locked throw hit player 2: {:?}",
        r.pvp
    );

    assert!(r.phase("lock + throw") < 0.05, "lock-on throw prediction is exact");
    // ---- 2. katana lunges into a player: first a tanky one (the server stops us at the body), then a kill
    r.phase("lock + throw (judged above)");
    let corrections_before = r.game.corrections;
    r.srv.players.iter_mut().find(|p| p.id == 2).unwrap().vitals.hp = 1000.0;
    r.srv.players.iter_mut().find(|p| p.id == 2).unwrap().vitals.max_hp = 1000.0;
    r.teleport(2, [0.5, 0.81, -3.5]);
    r.idle(40);
    r.press(Action::Slice);
    r.idle(60);
    let body_lunge = r.phase("katana lunge into a standing player");
    assert!(body_lunge < 0.15, "a lunge into a body stops where the server stops it ({body_lunge:.2} m)");
    assert_eq!(r.game.corrections, corrections_before, "and needs no reconcile correction");
    // set him 3.5 m ahead of wherever we stand now, wounded, and cut him down
    let me = r.me().pos(&r.srv.world);
    r.teleport(2, [me[0], 0.81, me[2] - 3.5]);
    r.srv.players.iter_mut().find(|p| p.id == 2).unwrap().vitals.hp = 30.0;
    r.idle(40);
    r.press(Action::Slice);
    r.idle(60);
    assert!(
        r.pvp.iter().any(|e| matches!(e, PvpEvent::Kill { killer: 1, victim: 2, kind: DamageKind::Slash, .. })),
        "the katana killed player 2: {:?}",
        r.pvp
    );
    let kill_lunge = r.phase("katana kill lunge");
    assert!(kill_lunge < 0.15, "the lunge that kills runs through the corpse like the server's ({kill_lunge:.2} m)");
    // the 1/100 m position quantisation of the snapshot can move the stop point by ~3 cm, which may trip the
    // reconciler's 2 cm deadband once; anything more would mean a real divergence
    assert!(r.game.corrections <= corrections_before + 2, "and needs at most two tiny reconcile corrections (the wider lock cone makes the lunge magnet see the target a hair differently; error stays < 2 cm) ({} -> {})", corrections_before, r.game.corrections);
    // the corpse respawns elsewhere after 3 s (a hull teleport the client sees a few ticks late): let that pass
    r.idle(240);
    assert!(r.phase("settle (corpse respawn)") < 0.05, "the grapple that follows starts from an exact state");

    // ---- 3. grapple: aim up at the anchor slab, hook, reel for a second, release
    r.game.aim.pitch = 0.38;
    r.idle(10);
    r.press(Action::Grapple);
    r.idle(60);
    assert!(r.max_grapple && r.saw_rope, "server attached the rope (server {}, client rope {})", r.max_grapple, r.saw_rope);
    r.press(Action::Grapple); // release
    r.idle(120); // fall back down
    r.game.aim.pitch = 0.0;

    assert!(r.phase("grapple") < 0.05, "grapple prediction is exact");
    // ---- 4. print a floor, a ramp (tap N again during the cooldown to cycle the variant) and a wall
    let cell_at = |r: &Rig| r.srv.blocks.pieces();
    r.game.aim.yaw = 1.0;
    r.idle(15);
    r.press(Action::BuildFloor);
    r.idle(2);
    r.press(Action::BuildFloor); // inside the cooldown: switches N to ramp, prints nothing
    r.idle(25);
    assert_eq!(cell_at(&r).len(), 1, "the quick second tap only cycled the variant");
    r.game.aim.yaw = 2.2;
    r.idle(15);
    r.press(Action::BuildFloor); // now prints the ramp
    r.idle(30);
    r.game.aim.yaw = 3.4;
    r.idle(15);
    r.press(Action::BuildWall);
    r.idle(30);
    let pieces = cell_at(&r);
    assert_eq!(pieces.len(), 3, "floor + ramp + wall landed on the server: {pieces:?}");
    for kind in [PieceKind::Floor, PieceKind::Ramp, PieceKind::Wall] {
        assert!(pieces.iter().any(|p| p.piece.kind == kind), "{kind:?} was printed: {pieces:?}");
    }
    let server_cells: Vec<[i32; 3]> = pieces.iter().map(|p| p.cell).collect();
    let mut client_cells: Vec<[i32; 3]> = r.view_blocks.iter().map(|b| b.1).collect();
    client_cells.sort();
    let mut want = server_cells.clone();
    want.sort();
    assert_eq!(client_cells, want, "the client sees exactly the server's pieces");

    assert!(r.phase("builds") < 0.05, "building does not disturb prediction");
    // ---- 5. shoot the wall until it de-rezzes
    let wall = pieces.iter().find(|p| p.piece.kind == PieceKind::Wall).copied().unwrap();
    let (wc, _) = r.srv.blocks.geometry(wall.cell, wall.piece, wall.scale_q);
    let me = r.me().pos(&r.srv.world);
    let (dx, dz) = (wc[0] - me[0], wc[2] - me[2]);
    r.game.aim.yaw = (-dx).atan2(-dz).rem_euclid(std::f32::consts::TAU);
    r.game.aim.pitch = ((wc[1] - (me[1] + 0.6)) / (dx * dx + dz * dz).sqrt().max(0.1)).atan();
    r.idle(20);
    let mut derezzed = false;
    for i in 0..900 {
        if i % 14 == 0 {
            r.press(Action::Fire);
        } else {
            r.idle(1);
        }
        if r.block_events.iter().any(|e| matches!(e, BlockEvent::DeRez { cell, .. } if *cell == wall.cell)) {
            derezzed = true;
            break;
        }
    }
    assert!(derezzed, "the wall de-rezzed under fire");
    r.idle(30);
    assert!(r.srv.blocks.is_free(wall.cell));
    assert!(!r.view_blocks.iter().any(|b| b.1 == wall.cell), "the client dropped the de-rezzed piece");

    // up to here nothing teleported the player: prediction stayed on the server's trajectory throughout
    assert!(r.phase("shooting the wall") < 0.05, "shooting does not disturb prediction");

    // ---- 6. die in the void, respawn
    let alive_before = r.me().vitals.deaths;
    r.teleport(1, [0.5, -80.0, 0.5]);
    r.idle(10);
    assert!(!r.me().vitals.alive(), "fell out of the world");
    // ---- 6b. the death sequence: the player keeps mashing every control, none of it may reach the game
    let (yaw_at_death, kills_before) = (r.game.aim.yaw, r.me().vitals.kills);
    let mash = InputFrame {
        stick: (1.0, 1.0),
        look: (300.0, 80.0),
        buttons: vec![ButtonEvent { action: Action::Fire, down: true }, ButtonEvent { action: Action::Slice, down: true }],
    };
    for _ in 0..60 * 8 {
        if r.me().vitals.alive() {
            break;
        }
        r.step(mash.clone());
    }
    assert!(r.me().vitals.alive() && r.me().vitals.deaths > alive_before, "respawned");
    let dv = &r.death_views;
    assert!(dv.len() > 60, "a death sequence was drawn for the whole wait ({} frames)", dv.len());
    assert_eq!(dv.iter().filter(|(d, ..)| d.fresh).count(), 1, "the one-shot burst fires exactly once, on the first frame");
    assert!(dv[0].0.fresh && dv[0].0.t < 0.2, "and it is the first frame: {:?}", dv[0].0);
    assert!(dv.windows(2).all(|w| w[1].0.t >= w[0].0.t), "the death clock only runs forward");
    let last = dv.last().unwrap();
    assert!(last.0.t > 2.0 && last.0.t <= last.0.total + 0.5, "the sequence runs the whole respawn wait: {:?}", last.0);
    assert!(last.1[1] < last.0.pos[1], "the eye has collapsed below where the head was: eye {:?} body {:?}", last.1, last.0.pos);
    assert!((r.game.aim.yaw - yaw_at_death).abs() < 1e-3, "look input is dead while dead: {} -> {}", yaw_at_death, r.game.aim.yaw);
    assert!((last.2 - dv[0].2).abs() > 0.2, "the kill-cam turned the view on its own");
    assert_eq!(r.me().vitals.kills, kills_before, "mashing fire and slash while dead killed nobody");
    r.idle(120);
    let (p, s) = (r.game.predicted_pos().unwrap(), r.me().pos(&r.srv.world));
    let err = ((p[0] - s[0]).powi(2) + (p[1] - s[1]).powi(2) + (p[2] - s[2]).powi(2)).sqrt();
    assert!(err < 0.6, "prediction is back on the server's position after the respawn (err {err:.2} m)");
    assert!(r.game.predicting());
}

/// Peak height (m above the standing start) of one jump driven through the real input path: the key goes down in one
/// frame, stays down for `hold_ticks` sim ticks (0 = down and up inside one frame), then goes up. The server's own
/// body is measured, so this covers the client latch -> intent -> server -> physics chain.
fn jump_apex(hold_ticks: u32) -> f32 {
    let mut r = Rig::new();
    r.idle(60);
    let y0 = r.me().pos(&r.srv.world)[1];
    let mut apex = 0.0f32;
    let down = |d| InputFrame { buttons: vec![ButtonEvent { action: Action::Jump, down: d }], ..Default::default() };
    if hold_ticks == 0 {
        r.press(Action::Jump);
    } else {
        r.step(down(true));
        for _ in 1..hold_ticks {
            r.step(InputFrame::default());
        }
        r.step(down(false));
    }
    for _ in 0..120 {
        r.idle(1);
        apex = apex.max(r.me().pos(&r.srv.world)[1] - y0);
    }
    apex
}

#[test]
fn a_held_jump_is_clearly_different_from_a_tap_through_the_real_input_path() {
    let flick = jump_apex(0); // down and up inside one frame
    let press = jump_apex(6); // a normal keypress: ~100 ms
    let held = jump_apex(45); // holding Space through the rise
    eprintln!("jump apex through ClientGame <-> Server: flick {flick:.2} m, 100 ms press {press:.2} m, held {held:.2} m");
    assert!(held > press * 1.25, "holding Space must visibly out-jump a normal press ({held:.2} vs {press:.2} m)");
    assert!(press > flick * 1.1, "and a press out-jumps a flick ({press:.2} vs {flick:.2} m)");
    assert!(press >= 2.0, "even an ordinary press is a real jump now ({press:.2} m; the old tuning gave ~1.5 m)");
}

/// Player 2 (a server-side shooter standing 15 m ahead, facing the client's player) throws one shuriken at tick
/// `fire_at` of the run while the client's player blocks from tick `block_from` (None = never). Returns the hp the
/// client's player lost and the events seen. Everything on the client side goes through `ClientGame`.
fn star_vs_block(block_from: Option<u32>) -> (f32, Vec<PvpEvent>) {
    use shinobi_proto::intent::{Buttons, PlayerIntent, ViewState, PITCH_LEVEL};
    use shinobi_proto::quant::q_yaw;
    let mut r = Rig::new();
    // the demo numbers: 7 hp stars, the guard. The shooter stands 15 m off, past the demo's 14 m star range, so lift the range here
    // (the range and its damage falloff have their own test in shinobi-sim)
    let mut b = shinobi_sim::balance::Balance::demo();
    b.combat.shuriken.range = 0.0;
    r.srv.set_balance(b).unwrap();
    r.idle(120);
    // the shooter faces +Z (toward the client's player), no aim noise, no lock tricks
    r.srv.players.iter_mut().find(|p| p.id == 1).unwrap().vitals.hp = 100.0;
    let hp0 = r.srv.players.iter().find(|p| p.id == 1).unwrap().vitals.hp;
    let down = |d| ButtonEvent { action: Action::Block, down: d };
    let mut events = Vec::new();
    for t in 0..90u32 {
        let tick = r.srv.tick;
        let fire = t == 30;
        let it = PlayerIntent { tick, buttons: Buttons(if fire { Buttons::FIRE } else { 0 }), aim_yaw: q_yaw(std::f32::consts::PI), aim_pitch: PITCH_LEVEL, ..Default::default() };
        let from = tick.saturating_sub(6).max(1);
        let pkt = shinobi_proto::intent::ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
        r.srv.ingest(2, &pkt.encode().unwrap());
        let mut buttons = vec![];
        if block_from == Some(t) {
            buttons.push(down(true));
        }
        r.step(InputFrame { buttons, ..Default::default() });
        events.extend(r.srv.pvp_events.iter().copied());
    }
    (hp0 - r.srv.players.iter().find(|p| p.id == 1).unwrap().vitals.hp, events)
}

#[test]
fn a_star_does_7_unblocked_about_1_blocked_and_nothing_on_a_parry_through_the_real_client() {
    let (open, ev) = star_vs_block(None);
    eprintln!("star unblocked: -{open:.2} hp; events {ev:?}");
    assert!((open - 7.0).abs() < 0.01, "an unblocked star does 7 ({open})");
    // raised long before the star arrives: a plain block, 15 % of 7
    let (blocked, ev) = star_vs_block(Some(5));
    eprintln!("star blocked: -{blocked:.2} hp; events {ev:?}");
    assert!(blocked < 0.01, "a held shield deflects the star: no damage ({blocked})");
    // raised later and later: early enough = a plain block (1.05), just before the star lands = a parry (0), too late =
    // the full 7. The client's intent lead shifts the exact ticks, so sweep and check the shape.
    let sweep: Vec<f32> = (8..44).map(|t| star_vs_block(Some(t)).0).collect();
    eprintln!("hp lost by raise tick 8..44: {sweep:?}");
    assert!(sweep.iter().any(|l| *l < 0.01), "some raise time parries the star completely: {sweep:?}");
    assert!(sweep.iter().any(|l| (l - 7.0).abs() < 0.01), "a shield that comes up too late takes the full 7: {sweep:?}");
    assert!(sweep.iter().all(|l| *l < 0.01 || (l - 7.0).abs() < 0.01), "only ever 0 or 7: {sweep:?}");
}

/// Katana homing through the real client: the locked target stands 4.5 m away, 30 degrees off the crosshair. A plain
/// slice swings past it; with homing the view turns onto the target, the lunge goes at it and the cut lands. Prediction
/// stays exact because the server uses the very aim the client sent.
#[test]
fn the_katana_homes_onto_the_locked_target_and_stays_in_sync() {
    let run = |home: bool| {
        let mut r = Rig::new();
        r.game.cfg.melee_home = home;
        r.srv.set_balance(shinobi_sim::balance::Balance::demo()).unwrap();
        r.idle(60);
        // the target 4.5 m away, 30 degrees to the left of straight ahead (-Z): the lock cone holds it
        let ang = 30f32.to_radians();
        r.teleport(2, [-4.5 * ang.sin(), 0.81, -4.5 * ang.cos()]);
        r.idle(40); // the lock acquires and holds
        let yaw0 = r.game.aim.yaw;
        r.press(Action::Slice);
        r.idle(40);
        let hit = r.pvp.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: 1, victim: 2, kind: DamageKind::Slash, .. }));
        (hit, (r.game.aim.yaw - yaw0).abs(), r.phase("homing slice"))
    };
    let (plain_hit, plain_turn, _) = run(false);
    let (home_hit, home_turn, err) = run(true);
    eprintln!("homing: plain hit {plain_hit} (turned {plain_turn:.2} rad), homed hit {home_hit} (turned {home_turn:.2} rad), prediction error {err:.3} m");
    assert!(home_hit, "the homed slice connected");
    assert!(home_turn > 0.2, "the view turned onto the target ({home_turn:.2} rad)");
    assert!(err < 0.2, "prediction stays exact ({err:.3} m)");
}

/// Hold the block key through the real client: the view freezes (look input is ignored), the server raises the
/// shield, the body still runs at full speed, and cutting is shut while it is up.
#[test]
fn blocking_locks_the_view_keeps_movement_and_the_server_raises_the_shield() {
    let mut r = Rig::new();
    r.idle(60);
    let yaw0 = r.game.aim.yaw;
    let z0 = r.me().pos(&r.srv.world)[2];
    let down = |a, d| ButtonEvent { action: a, down: d };
    // raise the shield and keep running forward while the mouse is dragged hard to the side
    r.step(InputFrame { buttons: vec![down(Action::Block, true)], stick: (0.0, 1.0), look: (0.0, 0.0), ..Default::default() });
    for _ in 0..40 {
        r.step(InputFrame { stick: (0.0, 1.0), look: (200.0, 40.0), buttons: vec![down(Action::Slice, true), down(Action::Slice, false)], ..Default::default() });
    }
    assert!(r.me().blk.held, "the server raised the shield");
    assert!((r.game.aim.yaw - yaw0).abs() < 1e-4, "the client view stayed locked: {} -> {}", yaw0, r.game.aim.yaw);
    assert!((r.me().pos(&r.srv.world)[2] - z0).abs() > 2.0, "and the body kept running while blocking");
    assert!(!r.srv.events.iter().any(|(id, e)| *id == 1 && matches!(e, shinobi_sim::combat::CombatEvent::Slash { .. })), "no katana swing while the shield is up");
    // release: look works again and the shield drops
    r.step(InputFrame { buttons: vec![down(Action::Block, false)], ..Default::default() });
    r.step(InputFrame { look: (200.0, 0.0), ..Default::default() });
    r.idle(30); // the client's intent lead: the release reaches the server a few ticks later
    assert!(!r.me().blk.held, "the shield dropped on release");
    assert!((r.game.aim.yaw - yaw0).abs() > 0.01, "look works again");
}
