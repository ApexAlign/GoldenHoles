//! End-to-end over real UDP: spawns the server binary, plays two fake clients against it.
//! Asserts handshake, snapshot delivery + delta baselines, reliable events, that position follows intents,
//! that strangers are ignored, and that a packet flood is kicked.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use shinobi_proto::bits::BitReader;
use shinobi_proto::datagram::{self, decode_challenge, decode_kick, encode_hello, KickReason, Welcome, TAG_CHALLENGE, TAG_KICK};
use shinobi_proto::gameplay::{Board, OwnExtras};
use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::dq_pos3;
use shinobi_proto::snapshot::{decode_delta, Snapshot};
use shinobi_sim::events::EventInbox;
use shinobi_sim::own_state::OwnState;

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_server() -> (ServerProc, String) {
    start_server_with(&[])
}

fn start_server_with(envs: &[(&str, &str)]) -> (ServerProc, String) {
    for _ in 0..10 {
        let (mut srv, addr) = spawn_server(envs);
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

fn spawn_server(envs: &[(&str, &str)]) -> (ServerProc, String) {
    // pick a free port; tiny race window is acceptable for a test
    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_shinobi-server"))
        .arg(&addr)
        // a networking test: pin the small arena so the shipped default (the big FFA map) cannot change its spawn spread
        .args(["--level", concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl")])
        .envs(envs.iter().copied())
        .stdout(Stdio::null())
        .stderr(if std::env::var("LB_DEBUG").is_ok() { Stdio::inherit() } else { Stdio::null() })
        .spawn()
        .expect("spawn server");
    (ServerProc(child), addr)
}

struct Client {
    sock: UdpSocket,
    /// Cookie from the server's CHALLENGE (valid for a couple of buckets: reused for same-address re-HELLOs).
    cookie: u64,
    id: u16,
    tick0: u32,
    started: Instant,
    seq: u16,
    snaps: Vec<Snapshot>,
    inbox: EventInbox,
    events_seen: usize,
    wall_events: usize,
    obstacle_events: usize,
    own_blocks: usize,
    own_acked: Option<u32>,
    hp_x10: Option<u16>,
    board: Option<Board>,
}

impl Client {
    fn connect(server: &str) -> Client {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(server).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 2048];
        let mut cookie = 0u64;
        loop {
            assert!(Instant::now() < deadline, "no WELCOME");
            sock.send(&encode_hello(cookie)).unwrap(); // cookie 0 first; idempotent afterwards
            if let Ok(n) = sock.recv(&mut buf) {
                if let Ok(c) = decode_challenge(&buf[..n]) {
                    cookie = c;
                    continue; // answer the challenge immediately
                }
                if let Ok(w) = Welcome::decode(&buf[..n]) {
                    sock.set_nonblocking(true).unwrap(); // pump() must never stall the 60 Hz send cadence
                    return Client {
                        sock,
                        cookie,
                        id: w.player_id,
                        tick0: w.server_tick,
                        started: Instant::now(),
                        seq: 0,
                        snaps: Vec::new(),
                        inbox: EventInbox::default(),
                        events_seen: 0,
                        wall_events: 0,
                        obstacle_events: 0,
                        own_blocks: 0,
                        own_acked: None,
                        hp_x10: None,
                        board: None,
                    };
                }
            }
        }
    }

    /// Our estimate of the server tick, slightly ahead so inputs are never stale.
    fn est_tick(&self) -> u32 {
        self.tick0 + (self.started.elapsed().as_secs_f64() * 60.0) as u32 + 3
    }

    fn send_intent(&mut self, move_x: i8, move_z: i8) {
        let t = self.est_tick();
        let mk = |tick| PlayerIntent { tick, buttons: Buttons(0), move_x, move_z, aim_yaw: 0, aim_pitch: 0, ultimate: false, build: false, build_wall: false, build_ramp: false, zip: false, block: false, mark: false, star: 0 };
        let pkt = ClientPacket {
            seq: self.seq,
            ack_snapshot_tick: self.snaps.last().map_or(0, |s| s.tick),
            ack_event_seq: self.inbox.ack(),
            view: ViewState { from_tick: t.saturating_sub(6), alpha: 0 },
            intents: (0..4).map(|i| mk(t - i)).collect(),
            report: None,
        };
        self.seq = self.seq.wrapping_add(1);
        self.sock.send(&pkt.encode().unwrap()).unwrap();
    }

    /// Drain and decode whatever snapshots have arrived.
    fn pump(&mut self) {
        let mut buf = [0u8; 2048];
        while let Ok(n) = self.sock.recv(&mut buf) {
            let Ok((events, own, board, snap)) = datagram::split_snapshot_v3(&buf[..n]) else { continue };
            if !own.is_empty() {
                let (extras, state) = OwnExtras::read(&mut BitReader::new(own), OwnState::read).expect("own block decodes");
                self.hp_x10 = Some(extras.hp_x10);
                if let Some(_state) = state {
                    self.own_acked = Some(extras.acked_tick);
                    self.own_blocks += 1;
                }
            }
            if !board.is_empty() {
                self.board = Some(Board::decode(board).expect("board decodes"));
            }
            let (mut count, mut walls, mut obstacles) = (0, 0, 0);
            self.inbox
                .read(&mut BitReader::new(events), |e| {
                    count += 1;
                    match e {
                        shinobi_sim::events::NetEvent::WallSpawn(_) => walls += 1,
                        shinobi_sim::events::NetEvent::ObstacleSpawn(_) => obstacles += 1,
                        _ => {}
                    }
                })
                .unwrap();
            self.events_seen += count;
            self.wall_events += walls;
            self.obstacle_events += obstacles;
            let known = &self.snaps;
            let s = decode_delta(snap, |t| known.iter().find(|s| s.tick == t)).expect("snapshot decodes");
            if self.snaps.last().map_or(true, |l| l.tick < s.tick) {
                self.snaps.push(s);
                if self.snaps.len() > 64 {
                    self.snaps.remove(0);
                }
            }
        }
    }

    fn pos_of(&self, id: u16) -> Option<[f32; 3]> {
        self.snaps.last()?.get(id).map(|e| dq_pos3(e.pos))
    }
}

fn horiz(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[test]
fn handshake_snapshots_and_position_follows_intents() {
    let (_srv, addr) = start_server();

    // a stranger's junk must not get a slot or a reply
    let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
    stranger.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    stranger.send_to(b"not a hello", &addr).unwrap();
    assert!(stranger.recv_from(&mut [0u8; 64]).is_err(), "stranger got a reply");

    let mut runner = Client::connect(&addr);
    let mut idler = Client::connect(&addr);
    assert_ne!(runner.id, idler.id);

    // settle: both stand still until the first snapshot so we have spawn positions
    let deadline = Instant::now() + Duration::from_secs(5);
    while runner.pos_of(runner.id).is_none() || runner.pos_of(idler.id).is_none() {
        assert!(Instant::now() < deadline, "no snapshot containing both players");
        runner.send_intent(0, 0);
        idler.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        runner.pump();
        idler.pump();
    }
    let start = runner.pos_of(runner.id).unwrap();
    let idle_start = runner.pos_of(idler.id).unwrap();

    // runner holds full stick until it has really moved (wait for the condition, not a fixed time: slow CI
    // hosts simulate fewer ticks per wall second); idler keeps sending neutral input
    let run_until = Instant::now() + Duration::from_secs(15);
    while Instant::now() < run_until && horiz(runner.pos_of(runner.id).unwrap(), start) <= 6.0 {
        runner.send_intent(0, -127);
        idler.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        runner.pump();
        idler.pump();
        if std::env::var("LB_DEBUG").is_ok() {
            eprintln!("t={} est={} snaps={} pos={:?}", runner.snaps.last().map_or(0, |s| s.tick), runner.est_tick(), runner.snaps.len(), runner.pos_of(runner.id));
        }
    }
    // both stand still; wait until both clients' newest snapshots agree on where the runner stopped
    let settle = Instant::now() + Duration::from_secs(5);
    loop {
        runner.send_intent(0, 0);
        idler.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        runner.pump();
        idler.pump();
        let (a, b) = (runner.pos_of(runner.id).unwrap(), idler.pos_of(runner.id).unwrap());
        if horiz(a, b) < 0.05 || Instant::now() > settle {
            break;
        }
    }

    // slow-cadence data (scoreboard once a second, own blocks, delta chain) arrives eventually: wait for it
    let data_by = Instant::now() + Duration::from_secs(8);
    while (runner.board.is_none() || runner.own_blocks <= 10 || runner.snaps.len() <= 10) && Instant::now() < data_by {
        runner.send_intent(0, 0);
        idler.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        runner.pump();
        idler.pump();
    }
    let moved = horiz(runner.pos_of(runner.id).unwrap(), start);
    let idled = horiz(runner.pos_of(idler.id).unwrap(), idle_start);
    assert!(moved > 3.0, "runner barely moved: {moved} m");
    assert!(idled < 0.5, "idler drifted {idled} m");

    // both clients see the same world state for the runner (server-authoritative, same snapshot stream)
    let (a, b) = (runner.pos_of(runner.id).unwrap(), idler.pos_of(runner.id).unwrap());
    assert!(horiz(a, b) < 1.5, "clients disagree on runner position: {a:?} vs {b:?}");

    // shooter: no trail walls, no obstacle waves by default. Reliable events (match flow...) are still acked contiguously
    assert_eq!((runner.wall_events, runner.obstacle_events), (0, 0), "no light walls / obstacles unless opted in");
    assert_eq!(runner.inbox.ack() as usize, runner.events_seen);
    // owner-only reconciliation block flows for the runner (it sent intents), and never for the idle-intent bot's
    // neighbour data: acked tick is a tick the server really executed (not ahead of its snapshots)
    assert!(runner.own_blocks > 10, "own blocks: {}", runner.own_blocks);
    let acked = runner.own_acked.unwrap();
    let newest = runner.snaps.last().unwrap().tick;
    assert!(acked <= newest + 1 && newest.saturating_sub(acked) < 30, "acked {acked} vs snapshot {newest}");
    // health is exact for the owner and the scoreboard flows (2 players, warmup)
    assert_eq!(runner.hp_x10, Some((shinobi_sim::balance::Balance::demo().player.max_hp * 10.0) as u16)); // full health in the server's demo preset
    let board = runner.board.as_ref().expect("board arrived");
    assert!(board.rows.iter().any(|r| r.id as u16 == runner.id) && board.rows.len() == 2, "{board:?}");
    // snapshots after the first are deltas against acked baselines and still decode (checked in pump via expect)
    assert!(runner.snaps.len() > 10, "{} snapshots", runner.snaps.len());
}

#[test]
fn packet_flood_gets_the_flooder_kicked_not_the_neighbour() {
    let (_srv, addr) = start_server();
    let mut victim = Client::connect(&addr);
    let flooder = Client::connect(&addr);
    let junk = vec![0xAAu8; 40];
    // ~3000 datagrams as fast as possible, far over the 120 pps cap
    for _ in 0..3000 {
        let _ = flooder.sock.send(&junk);
    }
    // keep the victim alive and watching; the flooder's player must vanish from snapshots
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        assert!(Instant::now() < deadline, "flooder was never removed from the world");
        victim.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        victim.pump();
        if let Some(s) = victim.snaps.last() {
            if s.get(flooder.id).is_none() && s.get(victim.id).is_some() {
                break;
            }
        }
    }
}

/// Waits until a KICK datagram shows up on `sock`, ignoring snapshots in between.
fn wait_kick(sock: &UdpSocket, within: Duration) -> Option<KickReason> {
    let deadline = Instant::now() + within;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        while let Ok(n) = sock.recv(&mut buf) {
            if buf[0] == TAG_KICK {
                return decode_kick(&buf[..n]).ok();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn flooder_is_told_why_and_cannot_rejoin_immediately() {
    let (_srv, addr) = start_server();
    let flooder = Client::connect(&addr);
    let junk = vec![0xAAu8; 40];
    for _ in 0..3000 {
        let _ = flooder.sock.send(&junk);
    }
    assert_eq!(wait_kick(&flooder.sock, Duration::from_secs(3)), Some(KickReason::Flood));
    // same address, immediate HELLO: still banned, told the reason, no WELCOME
    flooder.sock.send(&encode_hello(flooder.cookie)).unwrap();
    let mut buf = [0u8; 64];
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut reason = None;
    while Instant::now() < deadline && reason.is_none() {
        if let Ok(n) = flooder.sock.recv(&mut buf) {
            assert!(Welcome::decode(&buf[..n]).is_err(), "banned address was re-admitted");
            if buf[0] == TAG_KICK {
                reason = decode_kick(&buf[..n]).ok();
            }
        }
    }
    assert_eq!(reason, Some(KickReason::Flood));
}

#[test]
fn idle_timeout_kicks_then_same_address_gets_a_fresh_slot() {
    let (_srv, addr) = start_server_with(&[("SHINOBI_IDLE_MS", "400")]);
    let mut c = Client::connect(&addr);
    let first_id = c.id;
    // go silent
    assert_eq!(wait_kick(&c.sock, Duration::from_secs(3)), Some(KickReason::Timeout));
    // a departed client that keeps talking is told again (rate-limited), not ignored
    c.send_intent(0, 0);
    assert_eq!(wait_kick(&c.sock, Duration::from_secs(2)), Some(KickReason::Timeout));
    // reconnect from the same address: fresh slot, new player id, snapshots flow again
    c.sock.send(&encode_hello(c.cookie)).unwrap();
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut new_id = None;
    while Instant::now() < deadline && new_id.is_none() {
        if let Ok(n) = c.sock.recv(&mut buf) {
            if let Ok(w) = Welcome::decode(&buf[..n]) {
                new_id = Some(w.player_id);
            }
        }
    }
    let new_id = new_id.expect("no WELCOME after timeout");
    assert_ne!(new_id, first_id, "timed-out peer must not inherit its old slot");
    c.id = new_id;
    c.snaps.clear();
    let deadline = Instant::now() + Duration::from_secs(3);
    while c.pos_of(new_id).is_none() {
        assert!(Instant::now() < deadline, "no snapshot for the fresh slot");
        c.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        c.pump();
    }
}

#[test]
fn server_full_is_reported_not_silently_dropped() {
    let (_srv, addr) = start_server();
    let _clients: Vec<Client> = (0..16).map(|_| Client::connect(&addr)).collect();
    let late = UdpSocket::bind("127.0.0.1:0").unwrap();
    late.connect(&addr).unwrap();
    late.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    late.send(&encode_hello(0)).unwrap();
    let mut buf = [0u8; 64];
    let n = late.recv(&mut buf).expect("CHALLENGE");
    assert_eq!(buf[0], TAG_CHALLENGE);
    late.send(&encode_hello(decode_challenge(&buf[..n]).unwrap())).unwrap();
    late.set_nonblocking(true).unwrap();
    assert_eq!(wait_kick(&late, Duration::from_secs(2)), Some(KickReason::ServerFull));
}

#[test]
fn ai_bots_are_in_the_world_move_and_show_on_the_scoreboard() {
    let (_srv, addr) = start_server_with(&[("SHINOBI_BOTS", "3"), ("SHINOBI_BOT_SKILL", "easy")]);
    let mut c = Client::connect(&addr);
    let bot_ids = [201u16, 202, 203];
    let mut first: Option<Vec<[f32; 3]>> = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut moved = 0.0f32;
    while Instant::now() < deadline && moved < 1.0 {
        c.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        c.pump();
        let now: Option<Vec<[f32; 3]>> = bot_ids.iter().map(|id| c.pos_of(*id)).collect();
        if let Some(now) = now {
            let f = first.get_or_insert_with(|| now.clone());
            moved = f.iter().zip(&now).map(|(a, b)| horiz(*a, *b)).fold(0.0, f32::max);
        }
    }
    assert!(first.is_some(), "all three bots appeared in snapshots");
    assert!(moved >= 1.0, "bots are driven by the host: one moved {moved} m");
    // humans keep their own id range; bots show up on the board
    assert!(c.id >= 1 && c.id <= 200, "human id {}", c.id);
    let board = c.board.as_ref().expect("board");
    assert!(bot_ids.iter().all(|b| board.rows.iter().any(|r| r.id as u16 == *b)), "{board:?}");
}

#[test]
fn obstacle_waves_are_opt_in_and_reach_clients() {
    let (_srv, addr) = start_server_with(&[("SHINOBI_OBSTACLES", "1")]);
    let mut c = Client::connect(&addr);
    let deadline = Instant::now() + Duration::from_secs(5);
    while c.obstacle_events < 3 && Instant::now() < deadline {
        c.send_intent(0, 0);
        std::thread::sleep(Duration::from_millis(16));
        c.pump();
    }
    assert!(c.obstacle_events >= 3, "wave 0 was seeded/broadcast: {}", c.obstacle_events);
    assert_eq!(c.wall_events, 0, "obstacles do not bring light walls back");
}
