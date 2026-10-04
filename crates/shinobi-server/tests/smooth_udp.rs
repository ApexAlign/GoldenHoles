//! Presentation smoothness through the REAL stack: server binary over loopback UDP, `UdpGame` (wall-clock sync included),
//! `DisplayClock`, a Bevy-style fixed-step accumulator and ~144 Hz real-time frames, like the game's `publish_view`.
//! The own eye must glide every frame (a tick-stepped camera holds still on ~60 % of 144 Hz frames) and a hitch must not
//! teleport it.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shinobi_client::client_game::{ClientGameCfg, InputFrame};
use shinobi_client::display_clock::DisplayClock;
use shinobi_client::udp_game::UdpGame;
use shinobi_sim::level::LevelDesc;

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn arena() -> LevelDesc {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl");
    LevelDesc::from_text(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn start_server() -> (ServerProc, std::net::SocketAddr) {
    for _ in 0..10 {
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_shinobi-server"))
            .arg(addr.to_string())
            .args(["--level", concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        // ready when it answers a HELLO from a throwaway socket
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        probe.connect(addr).unwrap();
        probe.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let mut buf = [0u8; 64];
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ready = false;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            let _ = probe.send(&shinobi_proto::datagram::encode_hello(0));
            if probe.recv(&mut buf).is_ok() {
                ready = true;
                break;
            }
        }
        if ready {
            return (ServerProc(child), addr);
        }
        let _ = child.kill();
    }
    panic!("no server");
}

const TICK: f64 = 1.0 / 60.0;

/// Drives `frames` real-time frames of ~`frame_ms`; `hitch_at` frame sleeps `hitch_ms` extra. Returns per-frame (dt, eye xz).
fn drive(g: &mut UdpGame, frames: usize, frame_ms: u64, hitch_at: Option<(usize, u64)>) -> Vec<(f64, [f32; 2])> {
    let mut clock = DisplayClock::default();
    let mut acc = 0.0f64;
    let mut last = Instant::now();
    let mut out = Vec::new();
    for i in 0..frames {
        std::thread::sleep(Duration::from_millis(frame_ms));
        if let Some((at, ms)) = hitch_at {
            if i == at {
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;
        // the game's loop: the view is published first (First schedule), then the fixed steps run
        let sim_now = g.local_tick() + acc / TICK;
        let t = clock.advance(dt / TICK, sim_now);
        let v = g.view_at(t, sim_now);
        out.push((dt, [v.eye[0], v.eye[2]]));
        acc += dt;
        // forward for ~0.28 s, then back, so the run stays inside the arena
        let dir = if (i / 40) % 2 == 0 { 1.0 } else { -1.0 };
        let frame = InputFrame { stick: (0.0, dir), ..Default::default() };
        while acc >= TICK {
            g.fixed_step(&frame);
            acc -= TICK;
        }
    }
    out
}

fn joined() -> (ServerProc, UdpGame) {
    let (srv, addr) = start_server();
    let mut g = UdpGame::new(addr, arena(), ClientGameCfg::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !(g.connected() && g.game.net.interp.newest_tick().is_some()) {
        g.fixed_step(&InputFrame::default());
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(g.connected(), "joined");
    for _ in 0..60 {
        g.fixed_step(&InputFrame::default());
        std::thread::sleep(Duration::from_millis(16));
    }
    (srv, g)
}

fn moved(a: [f32; 2], b: [f32; 2]) -> f64 {
    (((b[0] - a[0]) as f64).powi(2) + ((b[1] - a[1]) as f64).powi(2)).sqrt()
}

#[test]
fn eye_glides_every_144hz_frame_through_the_real_udp_stack() {
    let (_srv, mut g) = joined();
    let tr = drive(&mut g, 340, 7, None);
    // frames where the player is clearly moving (skip the direction flips and the start-up)
    let mut flat = 0usize;
    let mut n = 0usize;
    let mut worst = 0.0f64;
    for i in 40..tr.len() - 1 {
        if i % 40 < 12 {
            continue; // direction flip: speed legitimately changes
        }
        n += 1;
        let d = moved(tr[i].1, tr[i + 1].1);
        if d < 1e-5 {
            flat += 1;
        }
        if d > 20.0 {
            continue; // the arena's edge hazard respawned the runner (a real teleport, not a presentation glitch)
        }
        worst = worst.max(d);
    }
    eprintln!("udp 144Hz: {flat}/{n} flat frames, worst frame step {worst:.3} m");
    assert!(flat * 20 < n, "interpolated eye holds still on {flat}/{n} frames (a tick-stepped camera does on ~60 %)");
    assert!(worst < 0.5, "no frame jumps half a metre at ~9 m/s and 144 Hz: {worst}");
}

#[test]
fn a_hitch_does_not_teleport_the_eye() {
    let (_srv, mut g) = joined();
    let tr = drive(&mut g, 150, 7, Some((80, 120)));
    let mut worst = 0.0f64;
    for i in 20..tr.len() - 1 {
        worst = worst.max(moved(tr[i].1, tr[i + 1].1));
    }
    eprintln!("udp hitch: worst frame step {worst:.3} m");
    // 9 m/s over the 0.13 s hitch frame is ~1.2 m of real travel; the smoothed clock spreads it, a raw one jumps it all
    assert!(worst < 1.3, "hitch frame step {worst}");
    assert!(g.connected(), "still connected after the hitch");
}
