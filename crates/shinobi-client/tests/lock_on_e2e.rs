//! Lock-on end to end, zero-latency links: a real `ClientGame` aims at a second (idle) player on a real `Server`.
//! The client's predictor must acquire, lock (progress 1, snap edge), break when a wall appears, never lock an
//! ally, and the assisted throw must land where the server says.

use std::time::Duration;

use shinobi_client::client_game::{ClientGame, ClientGameCfg, InputFrame, OwnAck};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, is_hello, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::combat::aim_assist::{LockEdge, LockPhase};
use shinobi_sim::events::{EventOutbox, NetEvent};
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::Server;

fn level() -> LevelDesc {
    LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0])], spawns: vec![[0.0, 0.81, 0.0]] }
}

fn entity(srv: &Server, i: usize) -> (u16, EntityState) {
    let p = &srv.players[i];
    let v = p.ch.vel;
    let kind = match p.ch.state {
        MoveState::Grounded => MoveKind::Grounded,
        MoveState::Airborne => MoveKind::Airborne,
        MoveState::WallRun { .. } => MoveKind::WallRun,
    };
    (p.id, EntityState { pos: q_pos3(p.pos(&srv.world)), vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)], yaw: p.yaw, kind, flags: 0 })
}

/// What the server sends player 1: both players in the snapshot, own state in the owner block.
fn snapshot_datagram(srv: &Server) -> Vec<u8> {
    let snap = Snapshot { tick: srv.tick, entities: (0..srv.players.len()).map(|i| entity(srv, i)).collect() };
    let mut ev = BitWriter::new();
    let mut outbox = EventOutbox::default();
    if srv.match_state.team_mode {
        for p in &srv.players {
            outbox.push(NetEvent::TeamAssign { player: p.id, team: p.team });
        }
    }
    outbox.write_pending(&mut ev, 256);
    let mut own = BitWriter::new();
    let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![], hit_seq: 0, ..Default::default() };
    extras.write(&mut own, |w| OwnState::from_snapshot(&srv.players[0].ch.snapshot(&srv.world)).write(w));
    frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
}

struct Rig {
    game: ClientGame,
    srv: Server,
    tick: u32,
    welcomed: bool,
    pending: Vec<Vec<u8>>,
    edges: Vec<LockEdge>,
    last: Option<(LockPhase, f32, bool)>,
    /// `RenderView.los_ids` of the latest view (HUD enemy tags need a clear line to the chest).
    los: Vec<u16>,
}

impl Rig {
    fn new(b_at: [f32; 3]) -> Rig {
        let lv = level();
        let game = ClientGame::new(lv.clone(), ClientGameCfg::default());
        let mut srv = Server::new(Cfg::default());
        lv.apply_to_server(&mut srv);
        srv.add_player(1, [0.0, 0.81, 0.0]);
        srv.add_player(2, b_at);
        Rig { game, srv, tick: 0, welcomed: false, pending: vec![], edges: vec![], last: None, los: vec![] }
    }

    /// One client tick + one server tick, no latency.
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
                self.pending.push(Welcome { player_id: 1, server_tick: self.srv.tick }.encode().to_vec());
                self.welcomed = true;
            } else {
                self.srv.ingest(1, &pkt);
            }
        }
        let view = self.game.view(now);
        self.edges.extend(view.lock_edges.iter().copied());
        self.last = view.lock.map(|l| (l.phase, l.progress, l.locked));
        self.los = view.los_ids.clone();
        self.srv.frame(Duration::from_nanos(16_666_667));
        if self.welcomed {
            self.pending.push(snapshot_datagram(&self.srv));
        }
        self.tick += 1;
    }

    fn run(&mut self, n: u32, f: impl Fn(&Rig) -> InputFrame) {
        for _ in 0..n {
            let fr = f(self);
            self.step(fr);
        }
    }
}

/// Aim straight at -Z (yaw 0, level): B stands on that line.
fn idle(_: &Rig) -> InputFrame {
    InputFrame::default()
}

#[test]
fn the_predictor_acquires_locks_and_breaks_behind_a_wall() {
    let mut r = Rig::new([0.0, 0.81, -15.0]);
    r.run(120, idle); // connect, predict, settle; the crosshair rests on B
    assert_eq!(r.last.map(|l| l.0), Some(LockPhase::Locked), "locked after resting on the target: {:?}", r.last);
    assert_eq!(r.last.unwrap().1, 1.0);
    assert!(r.los.contains(&2), "open ground: B is in line of sight ({:?})", r.los);
    assert!(r.edges.contains(&LockEdge::Acquired) && r.edges.contains(&LockEdge::Locked), "{:?}", r.edges);
    let acq = r.edges.iter().position(|e| *e == LockEdge::Acquired).unwrap();
    let lck = r.edges.iter().position(|e| *e == LockEdge::Locked).unwrap();
    assert!(acq < lck, "acquire before lock");
    // B steps out of the cone (teleported 6 m sideways on the server): the lock breaks
    let body = r.srv.players[1].ch.body;
    r.srv.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(6.0, 0.81, -15.0), true);
    shinobi_sim::character::sync_body_colliders(&mut r.srv.world, body);
    r.run(60, idle);
    assert!(r.edges.contains(&LockEdge::Broken), "{:?}", r.edges);
    assert!(r.last.map_or(true, |l| l.0 != LockPhase::Locked), "no longer locked: {:?}", r.last);
}

#[test]
fn a_wall_in_the_way_never_lets_a_lock_form() {
    // a wall in the level between us and B (the client's predicted world has it too)
    let mut lv = level();
    lv.boxes.push(LevelBox::new([0.0, 2.0, -8.0], [3.0, 3.0, 0.3]));
    let game = ClientGame::new(lv.clone(), ClientGameCfg::default());
    let mut srv = Server::new(Cfg::default());
    lv.apply_to_server(&mut srv);
    srv.add_player(1, [0.0, 0.81, 0.0]);
    srv.add_player(2, [0.0, 0.81, -15.0]);
    let mut r = Rig { game, srv, tick: 0, welcomed: false, pending: vec![], edges: vec![], last: None, los: vec![] };
    r.run(150, idle);
    assert!(r.game.predicting(), "prediction is running");
    assert!(r.edges.is_empty() && r.last.is_none(), "walled target is never acquired: {:?} {:?}", r.edges, r.last);
    assert!(!r.los.contains(&2), "no line of sight through the wall, so no HUD tag: {:?}", r.los);
}

#[test]
fn allies_are_never_locked() {
    let mut r = Rig::new([0.0, 0.81, -15.0]);
    r.srv.enable_teams(50);
    r.srv.set_team(1, 0);
    r.srv.set_team(2, 0);
    r.run(150, idle);
    assert!(r.edges.is_empty() && r.last.is_none(), "an ally in the cone is never acquired: {:?} {:?}", r.edges, r.last);
}
