//! The yumi through the REAL stack: server binary over loopback UDP, a pedestal in a custom level one step from the spawn,
//! `UdpGame` as the player. The client walks onto it (the whole server hears the call-out), holds the throw button (the
//! predicted draw ring fills, the walk slows), releases (the arrow appears in the snapshot, the arrow count drops in the
//! owner block), and the bow is spent after its last arrow.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shinobi_client::client_game::{Action, ButtonEvent, ClientGameCfg, InputFrame, RenderView};
use shinobi_client::udp_game::UdpGame;
use shinobi_sim::events::NetEvent;
use shinobi_sim::level::LevelDesc;
use shinobi_sim::power::PowerKind;
use shinobi_sim::power_pass::FX_PICKUP_YUMI;

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const LEVEL: &str = "box 0 -0.5 0 100 0.5 100\nspawn 0 0.81 0\nyumi 0 0.81 -3 45\ntag 0 0.81 -30 45\n";

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
fn pick_up_the_yumi_draw_loose_and_run_dry_through_the_real_server() {
    let dir = std::env::temp_dir().join(format!("shinobi-power-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("power.lvl");
    std::fs::write(&path, LEVEL).unwrap();
    // a one-player match goes live after a second, BEFORE the scenario (the match start resets everyone's hands and pedestals)
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

    // ---- walk onto the pedestal 3 m ahead
    let (mut callout, mut got) = (false, false);
    for _ in 0..240 {
        let v = step(&mut g, &InputFrame { stick: (0.0, 1.0), ..Default::default() });
        callout |= v.events.iter().any(|e| matches!(e, NetEvent::PowerFx { what, owner, .. } if *what == FX_PICKUP_YUMI && *owner == me));
        if let Some(p) = v.power.filter(|p| p.kind == PowerKind::Yumi) {
            got = true;
            assert_eq!(p.ammo, 3);
            break;
        }
    }
    assert!(got, "the client holds the yumi after walking onto it");
    for _ in 0..30 {
        let v = step(&mut g, &InputFrame::default());
        callout |= v.events.iter().any(|e| matches!(e, NetEvent::PowerFx { what, owner, .. } if *what == FX_PICKUP_YUMI && *owner == me));
    }
    assert!(callout, "the pickup call-out came through the event stream");

    // ---- draw: the held button fills the predicted ring (and the walk slows)
    let down = InputFrame { buttons: vec![ButtonEvent { action: Action::Fire, down: true }], ..Default::default() };
    let up = InputFrame { buttons: vec![ButtonEvent { action: Action::Fire, down: false }], ..Default::default() };
    let mut best_draw = 0.0f32;
    let mut full = false;
    let mut first = true;
    for _ in 0..150 {
        let v = step(&mut g, if first { &down } else { &none });
        first = false;
        if let Some(p) = v.power {
            best_draw = best_draw.max(p.draw);
            full |= p.full;
        }
        if full {
            break; // (a tick the client skips to re-time its intent lead does not advance the draw)
        }
    }
    assert!(best_draw > 0.99 && full, "the draw ring filled: {best_draw}");

    // ---- loose: an arrow shows up in the snapshot, the count drops to 2 in the owner block
    let mut arrow_seen = false;
    let mut ammo_after = 3;
    let mut first = true;
    for _ in 0..40 {
        let v = step(&mut g, if first { &up } else { &none });
        first = false;
        arrow_seen |= v.power_world.arrows.iter().any(|a| a.owner == me);
        if let Some(p) = v.power {
            ammo_after = p.ammo;
        }
    }
    assert!(arrow_seen, "the arrow flew through the snapshot");
    assert_eq!(ammo_after, 2, "one arrow spent");

    // ---- two more shots and the bow is gone
    for _ in 0..2 {
        for _ in 0..60 {
            step(&mut g, &InputFrame::default()); // cooldown
        }
        let mut first = true;
        for _ in 0..150 {
            let v = step(&mut g, if first { &down } else { &none });
            first = false;
            if v.power.map_or(false, |p| p.full) {
                break;
            }
        }
        step(&mut g, &up);
    }
    let mut v = step(&mut g, &InputFrame::default());
    for _ in 0..60 {
        v = step(&mut g, &InputFrame::default());
    }
    assert!(v.power.is_none(), "three arrows later the hands are empty again: {:?}", v.power);
    let _ = std::fs::remove_dir_all(&dir);
}
