//! Aim accuracy: a shuriken fired from the PREDICTED pose must fly through the crosshair ray the player was
//! looking along. Real `ClientGame` <-> real `Server`, zero latency, no sockets (see `e2e_prediction.rs` for the
//! lossy-link version). The crosshair ray is built from exactly what the shell draws: `RenderView::eye` and the
//! look angles from `RenderView::look_angles()`.

use std::time::Duration;

use shinobi_client::client_game::{Action, ButtonEvent, ClientGame, ClientGameCfg, InputFrame, OwnAck};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, is_hello, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::intent::ClientPacket;
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::events::EventOutbox;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::Server;

fn level() -> LevelDesc {
    level_with(false)
}

fn level_with(wall: bool) -> LevelDesc {
    let mut l = LevelDesc {
        boxes: vec![
            LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]),   // floor
            LevelBox::new([0.0, 20.0, -150.0], [200.0, 40.0, 1.0]), // far back wall, out of shuriken reach
        ],
        spawns: vec![[0.0, 0.81, 0.0]],
    };
    if wall {
        l.boxes.push(LevelBox::new([0.0, 12.0, -36.0], [60.0, 12.0, 1.0])); // hookable wall, face at z = -35
    }
    l
}

fn snapshot_datagram(srv: &Server) -> Vec<u8> {
    let p = &srv.players[0];
    let v = p.ch.vel;
    let st = EntityState {
        pos: q_pos3(p.pos(&srv.world)),
        vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)],
        yaw: p.yaw,
        kind: match p.ch.state {
            MoveState::Grounded => MoveKind::Grounded,
            MoveState::Airborne => MoveKind::Airborne,
            MoveState::WallRun { .. } => MoveKind::WallRun,
        },
        flags: 0,
    };
    let snap = Snapshot { tick: srv.tick, entities: vec![(p.id, st)] };
    let mut ev = BitWriter::new();
    EventOutbox::default().write_pending(&mut ev, 256);
    let mut own = BitWriter::new();
    let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![], hit_seq: 0, ..Default::default() };
    extras.write(&mut own, |w| OwnState::from_snapshot(&p.ch.snapshot(&srv.world)).write(w));
    frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
}

struct Rig {
    game: ClientGame,
    srv: Server,
    now: f64,
    down: Vec<Vec<u8>>,
}

impl Rig {
    fn new() -> Self {
        Self::with_wall(false)
    }

    fn with_wall(wall: bool) -> Self {
        let lv = level_with(wall);
        let mut srv = Server::new(Cfg::default());
        lv.apply_to_server(&mut srv);
        Self { game: ClientGame::new(lv, ClientGameCfg::default()), srv, now: 0.0, down: vec![] }
    }

    /// One 60 Hz tick with `f`; returns the view as drawn `alpha` of the way through the NEXT tick.
    fn tick(&mut self, f: &InputFrame, alpha: f64) -> shinobi_client::client_game::RenderView {
        for d in std::mem::take(&mut self.down) {
            let _ = self.game.on_datagram(&d, self.now);
        }
        if let Some(u) = self.game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                self.game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }
        let pkt = self.game.sim_tick(f, self.now);
        let view = self.game.view(self.now + alpha);
        if let Some(pkt) = pkt {
            if is_hello(&pkt) {
                if self.srv.players.is_empty() {
                    self.srv.add_player(1, level().spawn(0));
                }
                self.down.push(Welcome { player_id: 1, server_tick: self.srv.tick }.encode().to_vec());
            } else if !self.srv.players.is_empty() {
                let _ = ClientPacket::decode(&pkt).expect("valid packet");
                self.srv.ingest(1, &pkt);
            }
        }
        if !self.srv.players.is_empty() {
            self.srv.frame(Duration::from_nanos(16_666_667));
            self.down.push(snapshot_datagram(&self.srv));
        }
        self.now += 1.0;
        view
    }
}

fn btn(action: Action, down: bool) -> ButtonEvent {
    ButtonEvent { action, down }
}

/// Largest distance (m) any in-flight disc, once 4..30 m from the eye, strays from the crosshair ray drawn on the
/// frame the shot left. `setup` runs 240 ticks of movement first (`stick`, `hold` buttons, a look `(dx, dy)`).
fn miss(stick: (f32, f32), hold: &[Action], look: (f32, f32), alpha: f64) -> f32 {
    let mut rig = Rig::new();
    let mut f = InputFrame { stick, look, ..Default::default() };
    // ground movement ramps to full speed in ~0.13 s and a slide needs run speed: press the buttons after the ramp
    for _ in 0..15 {
        rig.tick(&f, 0.0);
        f.look = (0.0, 0.0);
    }
    for a in hold {
        f.buttons.push(btn(*a, true));
    }
    for _ in 0..225 {
        rig.tick(&f, 0.0);
        f.buttons.clear();
    }
    assert!(rig.game.predicting(), "prediction must be running");
    let fire = InputFrame { stick, buttons: vec![btn(Action::Fire, true), btn(Action::Fire, false)], ..Default::default() };
    let v0 = rig.tick(&fire, alpha);
    let (eye, (yaw, pitch)) = (v0.eye, v0.look_angles());
    let fwd = [-yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos()];
    let mut worst = 0.0f32;
    let mut seen = 0;
    let idle = InputFrame { stick, ..Default::default() };
    for _ in 0..30 {
        let v = rig.tick(&idle, 0.0);
        for (pos, _) in &v.discs {
            let rel = [pos[0] - eye[0], pos[1] - eye[1], pos[2] - eye[2]];
            let along = rel[0] * fwd[0] + rel[1] * fwd[1] + rel[2] * fwd[2];
            if !(4.0..30.0).contains(&along) {
                continue;
            }
            let perp = [rel[0] - fwd[0] * along, rel[1] - fwd[1] * along, rel[2] - fwd[2] * along];
            worst = worst.max((perp[0] * perp[0] + perp[1] * perp[1] + perp[2] * perp[2]).sqrt());
            seen += 1;
        }
    }
    assert!(seen > 0, "the shuriken must be in flight for the measurement");
    eprintln!("miss stick {stick:?} hold {} look {look:?} alpha {alpha}: {worst:.4} m ({seen} samples)", hold.len());
    worst
}

const TOL: f32 = 0.05; // m; a crosshair that is off by more than a few cm at 30 m feels broken

#[test]
fn standing_shot_goes_through_the_crosshair() {
    assert!(miss((0.0, 0.0), &[], (40.0, -25.0), 0.0) < TOL);
}

#[test]
fn running_shot_goes_through_the_crosshair() {
    assert!(miss((0.0, 1.0), &[], (-60.0, -30.0), 0.0) < TOL);
}

#[test]
fn strafing_shot_goes_through_the_crosshair() {
    assert!(miss((1.0, 0.0), &[], (0.0, 0.0), 0.0) < TOL);
}

#[test]
fn sliding_shot_goes_through_the_crosshair() {
    // the camera lowers the eye while sliding; the shot origin must follow it
    assert!(miss((0.0, 1.0), &[Action::Slide], (0.0, 10.0), 0.0) < TOL);
}

#[test]
fn mid_tick_draw_error_is_bounded_by_one_tick_of_travel() {
    // The shot leaves from the tick's pose; a frame drawn `alpha` into the tick shows the eye `alpha` ticks ahead.
    // That parallax can never exceed one tick of travel at run speed (9 m/s / 60 = 0.15 m).
    assert!(miss((1.0, 0.0), &[], (0.0, 0.0), 0.5) < 0.09);
}

#[test]
fn look_angles_are_exactly_the_aim_sent_in_intents() {
    let mut rig = Rig::new();
    let f = InputFrame { look: (123.0, -77.0), ..Default::default() };
    let mut last = None;
    for i in 0..200 {
        let v = rig.tick(&if i == 0 { f.clone() } else { InputFrame::default() }, 0.0);
        last = Some(v);
    }
    let v = last.unwrap();
    // recoil / bob / roll / look-kick must not leak into the aim: the view looks exactly along `aim`
    assert_eq!(v.look_angles(), (v.aim.yaw, v.aim.pitch));
}

#[test]
fn grapple_prediction_matches_the_server_and_pulls_the_player() {
    let mut rig = Rig::with_wall(true);
    let idle = InputFrame::default();
    for _ in 0..120 {
        rig.tick(&idle, 0.0);
    }
    // look up a little so the rope hits the wall above head height, then fire the wrist grapple
    let up = InputFrame { look: (0.0, -40.0), buttons: vec![btn(Action::Grapple, true), btn(Action::Grapple, false)], ..Default::default() };
    let mut roped = false;
    let mut worst = 0.0f32;
    let mut min_z = 0.0f32;
    for i in 0..150 {
        let v = rig.tick(&if i == 0 { up.clone() } else { idle.clone() }, 0.0);
        roped |= v.rope.is_some();
        worst = worst.max(rig.game.reconciler.visual_offset.iter().map(|d| d.abs()).fold(0.0, f32::max));
        min_z = min_z.min(rig.srv.players[0].pos(&rig.srv.world)[2]);
    }
    assert!(roped, "client predicted the rope");
    assert!(min_z < -12.0, "server pulled the player toward the wall: min z {min_z}");
    assert_eq!(rig.game.corrections, 0, "zero-latency prediction must never need a correction");
    assert!(worst < 0.01, "no visible reconcile offset: {worst}");
}
