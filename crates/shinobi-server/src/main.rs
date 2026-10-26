//! Headless authoritative server. `cargo run -p shinobi-server -- [BIND] [--port N] [--max-players N] [--level PATH] ...`
//! (see `config.rs`; effective config is printed at boot).
//!
//! Loop: drain UDP -> (cookie handshake | `Server::ingest`) -> `Server::frame(elapsed)` (0..=5 fixed 60 Hz steps)
//! -> replication (`Replicator::post_frame`) -> snapshot round every `SNAPSHOT_DIVISOR` ticks -> stats -> sleep.
//!
//! Handshake (stateless for strangers, see `shinobi_proto::datagram`): HELLO -> CHALLENGE(cookie) -> HELLO+cookie ->
//! slot + WELCOME. cookie = keyed hash(process secret, full source addr, time bucket); no per-address memory before it
//! verifies, so spoofed-source HELLOs cannot fill player slots.
//!
//! Session lifecycle: a peer leaves by `Kick(reason)` (cheat / flood), idle `Timeout`, or by itself. Departed
//! addresses are remembered for `DEPARTED_TTL`: datagrams from them get the `KICK` datagram again (so a stalled
//! mobile client learns immediately), and cheat/flood kicks are banned from re-joining for `BAN`.
//! Any other reconnect from the same address simply gets a fresh slot (new player id, fresh state).

mod config;
mod obstacles_spawner;
mod replicate;
mod setup;
mod stats;
mod supervise;
mod telemetry;

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use shinobi_proto::datagram::{self, encode_challenge, encode_kick, KickReason, Welcome, VERSION};
use shinobi_proto::intent::ClientPacket;
use shinobi_proto::{SNAPSHOT_DIVISOR, TICK_HZ};
use shinobi_sim::anticheat::{AntiCheatCfg, RateLimiter, Verdict};
use shinobi_sim::character::Cfg;
use shinobi_ai::BotHost;
use shinobi_sim::tick::Server;

use config::Config;
use replicate::{Features, Replicator};
use setup::Setup;
use stats::Stats;
use supervise::{guarded, make_bots, reset_world, BotPlan, PanicLog};

const MAX_DATAGRAM: usize = 512; // intents are ~30 bytes; anything bigger is junk

/// Sustained per-peer packet cap. An honest client sends <= 60/s (one per tick); the burst absorbs a
/// cellular radio stall delivering ~1 s of packets at once.
const PEER_PPS: f64 = 120.0;
const PEER_BURST: f64 = 60.0;
/// Cookie-VERIFIED joins per second (each allocates a slot and a physics body): tight.
const ADMIT_PPS: f64 = 20.0;
/// CHALLENGE / BadVersion replies to strangers. Replies are never larger than the HELLO that caused them, so this
/// only protects the sim thread's CPU. Kept generous: a spoofed flood spends these tokens, so a tight limit would
/// starve real joins (a real client retries every 100 ms for 5 s and needs ONE reply to get through).
const CHALLENGE_PPS: f64 = 4000.0;
/// Datagrams handled per loop pass: a flood can never starve the 60 Hz tick (the kernel drops the excess).
const MAX_DATAGRAMS_PER_PASS: usize = 8192;
/// At most this many strikes per frame from one flooder (kick_at is 8).
const MAX_FLOOD_STRIKES: u32 = 8;

/// Cheat / flood kicks cannot re-join for this long.
const BAN: Duration = Duration::from_secs(10);
/// How long a departed address keeps getting KICK replies.
const DEPARTED_TTL: Duration = Duration::from_secs(30);
/// KICK datagrams are sent this many times on departure (UDP loss).
const KICK_COPIES: usize = 3;

struct Peer {
    id: u16,
    last_heard: Instant,
    limiter: RateLimiter,
    /// Datagrams dropped by the limiter since the last strike pass.
    drops: u32,
}

struct Departed {
    reason: KickReason,
    at: Instant,
    banned_until: Option<Instant>,
    last_reply: Option<Instant>,
}

/// Handles contained panics: log, rebuild the world, and report whether it is a panic storm (the same
/// hostile input re-triggers it every frame), in which case the caller drops every peer to break the loop.
#[allow(clippy::too_many_arguments)]
fn recover(
    msgs: &mut Vec<String>,
    server: &mut Server,
    rep: &mut Replicator,
    cfg: Cfg,
    setup: &Setup,
    plan: &BotPlan,
    bots: &mut BotHost,
    peers: &HashMap<SocketAddr, Peer>,
    log: &mut PanicLog,
    stats: &mut Stats,
    now: Instant,
) -> bool {
    if msgs.is_empty() {
        return false;
    }
    let mut storm = false;
    for m in msgs.drain(..) {
        eprintln!("PANIC contained: {m}");
        stats.panics += 1;
        storm |= log.record(now);
    }
    let ids: Vec<u16> = peers.values().map(|p| p.id).collect();
    reset_world(server, rep, cfg, setup, &ids, plan, bots);
    eprintln!("world rebuilt after panic: {} player(s) respawned{}", ids.len(), if storm { ", panic storm: dropping all peers" } else { "" });
    storm
}

/// Keyed (per-process random SipHash keys) cookie over the FULL source address and the time bucket.
/// Never 0 (0 means "no cookie yet" on the wire).
fn cookie_for(keys: &RandomState, addr: SocketAddr, bucket: u64) -> u64 {
    keys.hash_one((addr, bucket)) | 1
}

fn send_kick(socket: &UdpSocket, addr: SocketAddr, reason: KickReason, copies: usize) {
    for _ in 0..copies {
        let _ = socket.send_to(&encode_kick(reason), addr);
    }
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let conf = match Config::parse(&args, &|k| std::env::var(k).ok()) {
        Ok(c) => c,
        Err(msg) if msg.is_empty() => {
            println!("{}", config::USAGE);
            return Ok(());
        }
        Err(msg) => {
            eprintln!("error: {msg}\n{}", config::USAGE);
            std::process::exit(2);
        }
    };
    // explicit --level wins; otherwise `levels/ffa_small.lvl` (else arena.lvl) when present; otherwise the built-in arena
    let level_path = conf.level.clone().or_else(shinobi_sim::level::default_level_path);
    let setup = match &level_path {
        Some(path) => Setup::load(path).unwrap_or_else(|e| {
            eprintln!("error: level: {e}");
            std::process::exit(2);
        }),
        None => Setup::standard(),
    };
    let setup = match &conf.balance {
        Some(path) => setup.with_balance_file(path).unwrap_or_else(|e| {
            eprintln!("error: balance: {e}");
            std::process::exit(2);
        }),
        None => setup,
    };
    let socket = UdpSocket::bind(&conf.bind)?;
    socket.set_nonblocking(true)?;
    // Prediction-config parity: the client compares these against its own level / balance and warns on a mismatch.
    let level_hash = setup.level_fingerprint();
    let balance_hash = setup.balance.fingerprint();
    eprintln!("shinobi-server: {}, {TICK_HZ} Hz, protocol v{VERSION}", conf.bind);
    eprintln!("{}", conf.describe());
    eprintln!("world: level {level_hash:016x}, balance {balance_hash:016x} (sent in WELCOME)");
    let level_summary = format!(
        "{} ({} boxes, {} spawns, {} pickups)",
        level_path.as_ref().map_or("built-in arena".to_string(), |p| p.display().to_string()),
        setup.level.boxes.len(),
        setup.level.spawns.len(),
        setup.pickups.len()
    );
    eprintln!("{}", conf.banner(&level_summary, config::lan_ip()));

    let mut setup = setup;
    setup.team_frag_limit = (conf.teams || conf.team_size > 0).then_some(config::TEAM_FRAG_LIMIT);
    let cfg = Cfg::default();
    let mut server = setup.build(cfg);
    let mut rep = Replicator::new(&cfg, conf.seed)
        .with_features(Features { obstacles: conf.obstacles });
    let plan = BotPlan { count: conf.bots, skill: conf.bot_skill, seed: conf.seed, team_size: conf.team_size };
    let mut bots = make_bots(&plan, &mut server);
    if plan.count > 0 {
        eprintln!("bots: {} AI opponent(s) ({}) joined as p{}..p{}", plan.count, plan.skill.as_str(), config::BOT_ID_BASE, config::BOT_ID_BASE as usize + plan.count - 1);
    }
    if plan.team_size > 0 {
        eprintln!("teams: {0}v{0} Cyan vs Orange, bots fill empty slots ({1})", plan.team_size, plan.skill.as_str());
    } else if setup.team_frag_limit.is_some() {
        eprintln!("teams: Cyan vs Orange (no bot fill)");
    }
    let ac = AntiCheatCfg::standard(&cfg);
    let epoch = Instant::now();
    let keys = RandomState::new();
    let bucket_ms = conf.cookie_bucket.as_millis().max(1);
    let mut challenge_limiter = RateLimiter::new(CHALLENGE_PPS, CHALLENGE_PPS * 2.0);
    let mut admit_limiter = RateLimiter::new(ADMIT_PPS, ADMIT_PPS * 2.0);
    let mut reply_limiter = RateLimiter::new(50.0, 100.0);
    let idle_timeout = conf.idle_timeout;

    let mut peers: HashMap<SocketAddr, Peer> = HashMap::new();
    let mut departed: HashMap<SocketAddr, Departed> = HashMap::new();
    let mut next_id: u16 = 1;
    let mut buf = [0u8; MAX_DATAGRAM + 1];
    let mut last = Instant::now();
    let mut stats = Stats::new(last, conf.stats_interval);
    let mut panic_log = PanicLog::default();
    let mut storm_pending = false;

    loop {
        let mut panic_msgs: Vec<String> = Vec::new();
        let mut handled = 0usize;
        while handled < MAX_DATAGRAMS_PER_PASS {
            handled += 1;
            match socket.recv_from(&mut buf) {
                Ok((n, addr)) if n <= MAX_DATAGRAM => {
                    stats.on_rx();
                    let pkt = &buf[..n];
                    let t = epoch.elapsed().as_secs_f64();
                    let now = Instant::now();
                    if let Some(p) = peers.get_mut(&addr) {
                        if !p.limiter.allow(t) {
                            p.drops += 1; // dropped before decode: a flood costs us one token check
                            stats.on_rate_drop();
                            continue;
                        }
                        p.last_heard = now;
                        if datagram::is_hello(pkt) {
                            // client retry: our WELCOME was lost
                            let w = Welcome { player_id: p.id, server_tick: server.tick, level_hash, balance_hash };
                            let _ = socket.send_to(&w.encode(), addr);
                            stats.on_tx();
                            continue;
                        }
                        if let Ok(cp) = ClientPacket::decode(pkt) {
                            rep.on_client_ack(p.id, cp.ack_snapshot_tick, cp.ack_event_seq);
                        }
                        let pid = p.id;
                        if let Err(msg) = guarded(|| server.ingest(pid, pkt)) {
                            panic_msgs.push(format!("ingest(player {pid}): {msg}"));
                        }
                    } else if let Ok(h) = datagram::decode_hello(pkt) {
                        // --- stateless handshake: nothing is remembered about `addr` until its cookie verifies ---
                        if h.version != VERSION {
                            if challenge_limiter.allow(t) {
                                send_kick(&socket, addr, KickReason::BadVersion, 1);
                                stats.on_tx();
                            }
                            continue;
                        }
                        let bucket = (epoch.elapsed().as_millis() / bucket_ms) as u64;
                        let valid = h.cookie != 0
                            && (h.cookie == cookie_for(&keys, addr, bucket)
                                || bucket.checked_sub(1).map_or(false, |b| h.cookie == cookie_for(&keys, addr, b)));
                        if !valid {
                            if challenge_limiter.allow(t) {
                                let _ = socket.send_to(&encode_challenge(cookie_for(&keys, addr, bucket)), addr);
                                stats.on_tx();
                            }
                            continue;
                        }
                        if !admit_limiter.allow(t) {
                            continue;
                        }
                        // banned? tell them why instead of admitting
                        if let Some(d) = departed.get(&addr) {
                            if d.banned_until.map_or(false, |b| now < b) {
                                send_kick(&socket, addr, d.reason, 1);
                                stats.on_tx();
                                continue;
                            }
                        }
                        if peers.len() >= conf.max_players {
                            send_kick(&socket, addr, KickReason::ServerFull, 1);
                            stats.on_tx();
                            continue;
                        }
                        // Human ids 1..=200 (bots own 201..=255): trail `owner` on the wire is a u8 and hazards stamp
                        // `player id as u8`. Round-robin so a fresh player does not instantly inherit a just-freed id.
                        const HUMAN_IDS: u16 = config::BOT_ID_BASE - 1;
                        let Some(id) = (0..HUMAN_IDS)
                            .map(|k| (next_id - 1 + k) % HUMAN_IDS + 1)
                            .find(|c| peers.values().all(|p| p.id != *c))
                        else {
                            continue;
                        };
                        next_id = id % HUMAN_IDS + 1;
                        departed.remove(&addr);
                        let at = server.pick_spawn(None);
                        server.add_player(id, at);
                        supervise::balance_human_team(&mut server, id);
                        rep.add_peer(&server, id, server.tick);
                        peers.insert(
                            addr,
                            Peer { id, last_heard: now, limiter: RateLimiter::new(PEER_PPS, PEER_BURST), drops: 0 },
                        );
                        let w = Welcome { player_id: id, server_tick: server.tick, level_hash, balance_hash };
                        let _ = socket.send_to(&w.encode(), addr);
                        stats.on_tx();
                        eprintln!("join {addr} as player {id}");
                    } else if let Some(d) = departed.get_mut(&addr) {
                        // a departed client still talking (stalled mobile app resuming): tell it, rate-limited
                        if d.last_reply.map_or(true, |r| now - r >= Duration::from_secs(1)) && reply_limiter.allow(t) {
                            d.last_reply = Some(now);
                            send_kick(&socket, addr, d.reason, 1);
                            stats.on_tx();
                        }
                    } // else: stranger without HELLO -> drop silently
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }

        let now = Instant::now();
        let t_sim = Instant::now();
        let elapsed = now - last;
        let report = match guarded(|| {
            supervise::fill_team_bots(&plan, &mut server, &mut bots);
            if plan.active() { bots.advance(&mut server, elapsed) } else { server.frame(elapsed) }
        }) {
            Ok(r) => r,
            Err(msg) => {
                panic_msgs.push(format!("frame: {msg}"));
                Default::default()
            }
        };
        let sim_cost = t_sim.elapsed();
        storm_pending |= recover(&mut panic_msgs, &mut server, &mut rep, cfg, &setup, &plan, &mut bots, &peers, &mut panic_log, &mut stats, now);
        last = now;
        if report.discarded > 0 {
            eprintln!("tick {}: stall, discarded {} steps", server.tick, report.discarded);
        }

        // --- who leaves this frame, and why ---
        let mut gone: Vec<(u16, KickReason)> = Vec::new();
        for line in server.strike_log.drain(..) {
            eprintln!("strike: {line}");
        }
        for id in &report.kicked {
            eprintln!("kick player {id} (cheat)");
            gone.push((*id, KickReason::Cheat));
        }
        if std::mem::take(&mut storm_pending) {
            for p in peers.values() {
                if !gone.iter().any(|g| g.0 == p.id) {
                    gone.push((p.id, KickReason::Other));
                }
            }
        }
        // flood -> strikes (same Guard as every other abuse path); Kick removes the peer below
        for p in peers.values_mut() {
            if p.drops == 0 {
                continue;
            }
            let n = std::mem::take(&mut p.drops).min(MAX_FLOOD_STRIKES);
            eprintln!("strike: player {} packet rate over {PEER_PPS} pkt/s (burst {PEER_BURST}): {n} datagram(s) dropped", p.id);
            if let Some(pl) = server.players.iter_mut().find(|pl| pl.id == p.id) {
                if pl.guard.strike_n(n, &ac) == Verdict::Kick && !gone.iter().any(|g| g.0 == p.id) {
                    eprintln!("kick player {} (packet flood)", p.id);
                    gone.push((p.id, KickReason::Flood));
                }
            }
        }
        for p in peers.values() {
            if now - p.last_heard > idle_timeout && !gone.iter().any(|g| g.0 == p.id) {
                eprintln!("timeout player {}", p.id);
                gone.push((p.id, KickReason::Timeout));
            }
        }
        for (id, reason) in &gone {
            if bots.is_bot(*id) {
                bots.remove_bot(&mut server, *id); // an AI the sim kicked: drop its brain too (humans handled below)
                continue;
            }
            server.remove_player(*id); // frees body + colliders; false (no-op) for cheat kicks `frame` already removed
            let tick = server.tick;
            rep.remove_peer(&mut server, *id, tick);
            match reason {
                KickReason::Timeout => stats.timeouts += 1,
                _ => stats.kicks += 1,
            }
            if let Some((addr, _)) = peers.iter().find(|(_, p)| p.id == *id) {
                let addr = *addr;
                send_kick(&socket, addr, *reason, KICK_COPIES);
                let banned_until = matches!(reason, KickReason::Cheat | KickReason::Flood).then(|| now + BAN);
                departed.insert(addr, Departed { reason: *reason, at: now, banned_until, last_reply: Some(now) });
            }
        }
        peers.retain(|_, p| !gone.iter().any(|g| g.0 == p.id));
        departed.retain(|_, d| now - d.at < DEPARTED_TTL);

        let t_repl = Instant::now();
        if report.ticks > 0 {
            if let Err(msg) = guarded(|| rep.post_frame(&mut server)) {
                panic_msgs.push(format!("replication: {msg}"));
            }
            if rep.due(server.tick, SNAPSHOT_DIVISOR) {
                let addr_of: HashMap<u16, SocketAddr> = peers.iter().map(|(a, p)| (p.id, *a)).collect();
                for (id, bytes) in rep.build_round(&server) {
                    if let Some(a) = addr_of.get(&id) {
                        let _ = socket.send_to(&bytes, a);
                        stats.on_tx();
                    }
                }
            }
        }
        let repl_cost = t_repl.elapsed();
        storm_pending |= recover(&mut panic_msgs, &mut server, &mut rep, cfg, &setup, &plan, &mut bots, &peers, &mut panic_log, &mut stats, now);

        for line in rep.take_log() {
            eprintln!("{line}");
        }
        let (text, json) = rep.take_telemetry();
        for line in &text {
            eprintln!("{line}");
        }
        if let (Some(path), false) = (&conf.telemetry, text.is_empty()) {
            use std::io::Write;
            let stamp = format!("# {:.0}s since server start", epoch.elapsed().as_secs_f64());
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            let write = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| {
                writeln!(f, "{stamp}")?;
                for l in text.iter().chain(json.iter()) {
                    writeln!(f, "{l}")?;
                }
                Ok(())
            });
            if let Err(e) = write {
                eprintln!("telemetry: cannot write {}: {e}", path.display());
            }
        }
        if let Some(line) = stats.on_frame(now, report.ticks, sim_cost, repl_cost) {
            eprintln!("{line}");
        }
        if let Some(line) = stats.maybe_report(now, server.tick, peers.len()) {
            eprintln!("{line}");
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}
