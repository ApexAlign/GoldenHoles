//! Cookie handshake end-to-end over real UDP: stateless for strangers, bound to source address, time-limited,
//! never amplifying, version-checked. Spoofed-source slot exhaustion must not lock out real players.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shinobi_proto::datagram::{
    decode_challenge, decode_kick, encode_hello, KickReason, Welcome, CHALLENGE_LEN, HELLO_LEN, TAG_CHALLENGE, TAG_KICK,
    VERSION,
};

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_server(args: &[&str], envs: &[(&str, &str)]) -> (ServerProc, String) {
    // the free port is only probed, not held: another test may grab it first, then the server exits -> retry
    for _ in 0..10 {
        let (mut srv, addr) = spawn_server(args, envs);
        if wait_ready(&mut srv.0, &addr) {
            return (srv, addr);
        }
    }
    panic!("no free port for the server");
}

/// Blocks until the spawned server answers a HELLO (probe from a throwaway socket: no player slot is taken), so
/// process start-up time under load never eats into a test's own timeouts. `false` = it exited (port taken).
fn wait_ready(child: &mut Child, addr: &str) -> bool {
    let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
    probe.connect(addr).unwrap();
    probe.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let mut buf = [0u8; 64];
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return false;
        }
        let _ = probe.send(&shinobi_proto::datagram::encode_hello(0));
        if probe.recv(&mut buf).is_ok() {
            return true;
        }
    }
    panic!("server never answered a HELLO");
}

fn spawn_server(args: &[&str], envs: &[(&str, &str)]) -> (ServerProc, String) {
    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_shinobi-server"))
        .arg(&addr)
        .args(args)
        .envs(envs.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    (ServerProc(child), addr)
}

fn sock(server: &str) -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    s.connect(server).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(100))).unwrap(); // = HELLO_RETRY_MS: retry like a real client
    s
}

enum Reply {
    Challenge(u64, usize),
    Welcome(u16),
    Kick(KickReason),
    Nothing,
}

/// Discards replies still queued from earlier sends: under load a retry's answer can arrive after the next
/// exchange began and would be mistaken for its reply.
fn drain(s: &UdpSocket) {
    let mut buf = [0u8; 2048];
    s.set_nonblocking(true).unwrap();
    while s.recv(&mut buf).is_ok() {}
    s.set_nonblocking(false).unwrap();
}

/// Sends `bytes`, returns the first meaningful reply (retrying the send for server start-up).
fn exchange(s: &UdpSocket, bytes: &[u8]) -> Reply {
    drain(s);
    let reply = exchange_once(s, bytes);
    // late duplicates of this reply (from retries) must not leak into the next exchange
    std::thread::sleep(Duration::from_millis(30));
    drain(s);
    reply
}

fn exchange_once(s: &UdpSocket, bytes: &[u8]) -> Reply {
    let deadline = Instant::now() + Duration::from_millis(shinobi_proto::datagram::CONNECT_TIMEOUT_MS);
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        s.send(bytes).unwrap();
        if let Ok(n) = s.recv(&mut buf) {
            if buf[0] == TAG_CHALLENGE {
                return Reply::Challenge(decode_challenge(&buf[..n]).unwrap(), n);
            }
            if buf[0] == TAG_KICK {
                return Reply::Kick(decode_kick(&buf[..n]).unwrap());
            }
            if let Ok(w) = Welcome::decode(&buf[..n]) {
                return Reply::Welcome(w.player_id);
            }
        }
    }
    Reply::Nothing
}

fn full_join(server: &str) -> (UdpSocket, u16) {
    let s = sock(server);
    let Reply::Challenge(cookie, _) = exchange(&s, &encode_hello(0)) else { panic!("expected CHALLENGE") };
    match exchange(&s, &encode_hello(cookie)) {
        Reply::Welcome(id) => (s, id),
        Reply::Kick(r) => panic!("refused: {r:?}"),
        _ => panic!("no WELCOME after echoing the cookie"),
    }
}

#[test]
fn hello_gets_a_small_challenge_and_only_the_echoed_cookie_gets_a_slot() {
    let (_srv, addr) = start_server(&[], &[]);
    let s = sock(&addr);
    let Reply::Challenge(cookie, len) = exchange(&s, &encode_hello(0)) else { panic!("expected CHALLENGE") };
    assert!(len == CHALLENGE_LEN && len <= HELLO_LEN, "CHALLENGE must never be larger than the HELLO");
    assert_ne!(cookie, 0);
    // a wrong cookie is just challenged again (no slot, no WELCOME)
    assert!(matches!(exchange(&s, &encode_hello(cookie ^ 0xFFFF)), Reply::Challenge(..)));
    assert!(matches!(exchange(&s, &encode_hello(cookie)), Reply::Welcome(_)));
}

#[test]
fn cookie_is_bound_to_the_source_address() {
    let (_srv, addr) = start_server(&[], &[]);
    let (a, b) = (sock(&addr), sock(&addr));
    let Reply::Challenge(cookie_a, _) = exchange(&a, &encode_hello(0)) else { panic!() };
    // an attacker (or a different client) replays A's cookie from address B: rejected, B is challenged afresh
    match exchange(&b, &encode_hello(cookie_a)) {
        Reply::Challenge(cookie_b, _) => assert_ne!(cookie_a, cookie_b, "cookies are per address"),
        Reply::Welcome(_) => panic!("cookie replayed from another address was accepted"),
        _ => panic!("expected a fresh CHALLENGE"),
    }
    // the rightful owner still gets in
    assert!(matches!(exchange(&a, &encode_hello(cookie_a)), Reply::Welcome(_)));
}

#[test]
fn cookie_expires_after_two_time_buckets() {
    let (_srv, addr) = start_server(&["--cookie-bucket-ms", "200"], &[]);
    let s = sock(&addr);
    let Reply::Challenge(old, _) = exchange(&s, &encode_hello(0)) else { panic!() };
    std::thread::sleep(Duration::from_millis(550)); // > 2 buckets
    let Reply::Challenge(fresh, _) = exchange(&s, &encode_hello(old)) else { panic!("stale cookie accepted") };
    assert_ne!(old, fresh);
    assert!(matches!(exchange(&s, &encode_hello(fresh)), Reply::Welcome(_)));
}

#[test]
fn wrong_protocol_version_is_told_so() {
    let (_srv, addr) = start_server(&[], &[]);
    let s = sock(&addr);
    let mut hello = encode_hello(0);
    hello[4] = VERSION + 1;
    assert!(matches!(exchange(&s, &hello), Reply::Kick(KickReason::BadVersion)));
}

#[test]
fn spoofed_hello_flood_neither_fills_slots_nor_blocks_a_real_join() {
    // 4 slots; 26 "spoofed" sources (ports A..Z) hammer HELLO and never echo a cookie: ~10k pps, 2-3x the
    // server's CHALLENGE budget, so real clients must win through retries, exactly like the shipped client does.
    let (_srv, addr) = start_server(&["--max-players", "4"], &[]);
    let stop = Arc::new(AtomicBool::new(false));
    let flooders: Vec<_> = (0..26)
        .map(|_| {
            let (addr, stop) = (addr.clone(), stop.clone());
            std::thread::spawn(move || {
                let s = sock(&addr);
                s.set_nonblocking(true).unwrap();
                while !stop.load(Ordering::Relaxed) {
                    let _ = s.send(&encode_hello(0));
                    let _ = s.send(&encode_hello(0xDEAD_BEEF)); // forged cookie too
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300)); // flood is in full swing

    // real players join DURING the flood, all four slots, no ServerFull
    let joined: Vec<_> = (0..4).map(|_| full_join(&addr)).collect();
    let mut ids: Vec<_> = joined.iter().map(|j| j.1).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "all four real players joined while the flood was running");
    stop.store(true, Ordering::Relaxed);
    for f in flooders {
        f.join().unwrap();
    }
    // slots were never consumed by the spoofers: the fifth real player is refused only because the 4 REAL ones hold them
    let s = sock(&addr);
    let Reply::Challenge(c, _) = exchange(&s, &encode_hello(0)) else { panic!() };
    assert!(matches!(exchange(&s, &encode_hello(c)), Reply::Kick(KickReason::ServerFull)));
}
