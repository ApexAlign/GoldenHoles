//! The yumi through the REAL stack: server binary over loopback UDP, `UdpGame` as the player. Everyone has the bow from
//! spawn (no pickup): the client raises it (key 2), holds the slice button (the predicted draw ring fills, the walk slows),
//! releases (the arrow appears in the snapshot, the reload bar empties), and the bow is loaded again 1.1 s later.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shinobi_client::client_game::{Action, ButtonEvent, ClientGameCfg, InputFrame, RenderView};
use shinobi_client::udp_game::UdpGame;
use shinobi_sim::level::LevelDesc;
use shinobi_sim::power::PowerKind;

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const LEVEL: &str = "box 0 -0.5 0 100 0.5 100\nspawn 0 0.81 0\nyumi 0 0.81 -3 45\ntag 0 0.81 -30 45\n"; // (the yumi line is a dead pedestal)

fn start_server(path: &std::path::Path, balance: &std::path::Path) -> (ServerProc, std::net::SocketAddr) {
    for _ in 0..10 {
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_shinobi-server")).arg(addr.to_string()).args(["--level", path.to_str().unwrap(), "--balance", balance.to_str().unwrap()]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn server");
        let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
        probe.connect(addr).unwrap();
        probe.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let mut buf = [0u8; 64];
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            let _ = probe.send(&shinobi_proto::datagram::encode_hello(0));
            if probe.recv(&mut buf).is_ok() {
                return (ServerProc(child), addr);
            }
        }
        let _ = child.kill();
    }
    panic!("no server");
}

fn step(g: &mut UdpGame, frame: &InputFrame) -> RenderView {
    g.fixed_step(frame);
    std::thread::sleep(Duration::from_millis(16));
    g.view()
}

#[test]
fn everyone_has_the_bow_draw_loose_and_reload_through_the_real_server() {
    let dir = std::env::temp_dir().join(format!("shinobi-power-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("power.lvl");
    std::fs::write(&path, LEVEL).unwrap();
    // a one-player match goes live after a second, BEFORE the scenario (the match start resets everyone's hands)
    let bal = dir.join("balance.txt");
    std::fs::write(&bal, "match.warmup_ticks = 60\nmatch.min_players = 1\n").unwrap();
    let (_srv, addr) = start_server(&path, &bal);
    let mut g = UdpGame::new(addr, LevelDesc::from_text(LEVEL).unwrap(), ClientGameCfg::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !(g.connected() && g.game.net.interp.newest_tick().is_some()) {
        step(&mut g, &InputFrame::default());
    }
    assert!(g.connected(), "joined");
    for _ in 0..90 {
        step(&mut g, &InputFrame::default());
    }
    let me = g.own_id().unwrap();
    let none = InputFrame::default();

    // ---- the bow is in hand from the start: loaded, nothing to pick up (the dead yumi pedestal stays dead)
    let v = step(&mut g, &none);
    let p = v.power.expect("a bow view from spawn");
    assert_eq!((p.kind, p.reload, p.draw), (PowerKind::Yumi, 1.0, 0.0), "loaded and ready");
    assert_eq!(g.game.net.power_report().map(|r| r.0), Some(PowerKind::Yumi as u8), "the server reports the bow");

    // ---- raise it (key 2) and hold the draw (left click): the predicted ring fills
    let raise = InputFrame { buttons: vec![ButtonEvent { action: Action::Bow, down: true }, ButtonEvent { action: Action::Slice, down: true }], ..Default::default() };
    let release = InputFrame { buttons: vec![ButtonEvent { action: Action::Slice, down: false }], ..Default::default() };
    let mut best_draw = 0.0f32;
    let mut full = false;
    let mut first = true;
    for _ in 0..150 {
        let v = step(&mut g, if first { &raise } else { &none });
        first = false;
        if let Some(p) = v.power {
            best_draw = best_draw.max(p.draw);
            full |= p.full;
            assert!(p.up, "the HUD knows the bow is up");
        }
        if full {
            break; // (a tick the client skips to re-time its intent lead does not advance the draw)
        }
    }
    assert!(best_draw > 0.99 && full, "the draw ring filled: {best_draw}");

    // ---- loose: an arrow shows up in the snapshot and the reload bar empties
    let (mut arrow_seen, mut min_reload) = (false, 1.0f32);
    let mut first = true;
    for _ in 0..40 {
        let v = step(&mut g, if first { &release } else { &none });
        first = false;
        arrow_seen |= v.power_world.arrows.iter().any(|a| a.owner == me);
        if let Some(p) = v.power {
            min_reload = min_reload.min(p.reload);
        }
    }
    assert!(arrow_seen, "the arrow flew through the snapshot");
    assert!(min_reload < 0.5, "the reload bar emptied after the shot: {min_reload}");

    // ---- 1.1 s later the bow is loaded again and a second draw starts
    for _ in 0..200 {
        step(&mut g, &none);
    }
    let v = step(&mut g, &none);
    assert_eq!(v.power.map(|p| p.reload), Some(1.0), "reloaded: {:?}", v.power);
    let draw = InputFrame { buttons: vec![ButtonEvent { action: Action::Slice, down: true }], ..Default::default() };
    let mut drawing = false;
    let mut first = true;
    for _ in 0..30 {
        let v = step(&mut g, if first { &draw } else { &none });
        first = false;
        drawing |= v.power.map_or(false, |p| p.draw > 0.0);
    }
    assert!(drawing, "a second draw begins once reloaded");
    let _ = std::fs::remove_dir_all(&dir);
}
