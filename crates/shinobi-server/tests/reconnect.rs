//! Live reconnect over real UDP: the server binary kicks a flooding client, the client's `UdpGame::reconnect` (what the
//! game's "press Enter" does) rejoins from a fresh socket, and the new session predicts, moves and receives snapshots.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shinobi_client::client_game::{ClientGameCfg, InputFrame};
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

/// Steps the client at ~60 Hz until `done` or the deadline.
fn run_until(g: &mut UdpGame, frame: &InputFrame, secs: u64, mut done: impl FnMut(&mut UdpGame) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        g.fixed_step(frame);
        if done(g) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    false
}

#[test]
fn kicked_client_reconnects_and_the_new_session_plays() {
    let (_srv, addr) = start_server();
    let mut g = UdpGame::new(addr, arena(), ClientGameCfg::default()).unwrap();
    assert!(run_until(&mut g, &InputFrame::default(), 8, |g| g.connected() && g.game.net.interp.newest_tick().is_some()), "first join");
    let first_id = g.own_id().unwrap();

    // flood the server from this session's socket: far over the per-peer packet cap -> Flood kick
    let junk = [0xAAu8; 40];
    for _ in 0..3000 {
        let _ = g.send_raw(&junk);
    }
    assert!(run_until(&mut g, &InputFrame::default(), 5, |g| g.game.net.kicked().is_some()), "the server kicked the flooder");

    // press Enter: a fresh session from a new socket (the ban is per address, so it is admitted at once)
    g.reconnect(arena(), ClientGameCfg::default()).unwrap();
    assert!(g.game.net.kicked().is_none());
    assert!(run_until(&mut g, &InputFrame::default(), 8, |g| g.connected() && g.game.net.interp.newest_tick().is_some()), "rejoined and receiving snapshots");
    let _ = first_id; // the id may be reused; what matters is that the session works

    // input, prediction and the camera all work: holding forward moves the predicted player
    let start = g.view().local_pos.expect("a predicted position");
    let fwd = InputFrame { stick: (0.0, 1.0), ..Default::default() };
    assert!(run_until(&mut g, &fwd, 6, |g| {
        let v = g.view();
        v.local_pos.map_or(false, |p| ((p[0] - start[0]).powi(2) + (p[2] - start[2]).powi(2)).sqrt() > 1.0)
    }), "the reconnected player moves under input");
    let v = g.view();
    assert!(v.connected && v.kicked.is_none());
}
