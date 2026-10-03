//! `shinobi-bot --addr 127.0.0.1:7777 --count 16 --behaviour mixed --secs 60`
//! Spawns N scripted bots against a running server, prints a per-bot line and totals.
//! Exit status is non-zero on any kick, failed connect, or undecodable datagram.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shinobi_bot::{run, Behaviour, BotConfig};

fn main() {
    let mut addr = "127.0.0.1:7777".to_string();
    let mut count = 1u32;
    let mut behaviour = Behaviour::Mixed;
    let mut secs = 10.0f64;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().unwrap_or_else(|| usage(&format!("{name} needs a value")));
        match a.as_str() {
            "--addr" => addr = val("--addr"),
            "--count" => count = val("--count").parse().unwrap_or_else(|_| usage("bad --count")),
            "--secs" => secs = val("--secs").parse().unwrap_or_else(|_| usage("bad --secs")),
            "--behaviour" => {
                behaviour = Behaviour::parse(&val("--behaviour"))
                    .unwrap_or_else(|| usage("behaviour: circles | strafe | spam | hook | mixed"))
            }
            "-h" | "--help" => usage(""),
            other => usage(&format!("unknown argument {other}")),
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let handles: Vec<_> = (0..count)
        .map(|i| {
            let (stop, cfg) = (stop.clone(), BotConfig::new(addr.clone(), behaviour, i));
            let h = std::thread::spawn(move || run(&cfg, &stop));
            std::thread::sleep(Duration::from_millis(30)); // stagger joins; the server rate-limits HELLOs
            h
        })
        .collect();

    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end && !handles.iter().all(|h| h.is_finished()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);

    let mut bad = 0;
    let mut failed = false;
    println!("{:>3} {:>6} {:>8} {:>8} {:>7} {:>6} {:>6} {:>6} {:>5} {:>6} {:>8}  status", "bot", "id", "sent", "recv", "snaps", "ropes", "blocks", "obst", "team", "known", "srv_hz");
    let (mut ropes, mut blocks, mut teams, mut known) = (0u64, 0u64, [0u32; 2], 0u32);
    for h in handles {
        let r = h.join().expect("bot thread panicked");
        bad += r.bad_datagrams;
        let status = match (r.connect_failed, r.kicked, r.bad_datagrams) {
            (true, _, _) => "CONNECT FAILED".to_string(),
            (_, Some(k), _) => format!("KICKED ({})", k.as_str()),
            (_, _, b) if b > 0 => format!("{b} bad datagrams (last: {})", r.last_error.as_deref().unwrap_or("?")),
            _ => "ok".to_string(),
        };
        failed |= status != "ok";
        ropes += r.rope_attaches;
        blocks += r.blocks_placed;
        known = known.max(r.teams_known);
        if let Some(t) = r.team.filter(|t| *t < 2) {
            teams[t as usize] += 1;
        }
        println!(
            "{:>3} {:>6} {:>8} {:>8} {:>7} {:>6} {:>6} {:>6} {:>5} {:>6} {:>8}  {status}",
            r.index,
            r.player_id.map_or("-".into(), |i| i.to_string()),
            r.packets_sent,
            r.datagrams_recv,
            r.snapshots,
            r.rope_attaches,
            r.blocks_placed,
            r.obstacle_events,
            r.team.map_or("-".into(), |t| ["cyan", "orange"].get(t as usize).copied().unwrap_or("?").to_string()),
            r.teams_known,
            r.server_hz().map_or("-".into(), |h| format!("{h:.2}")),
        );
    }
    println!("bad datagrams total: {bad}");
    println!("totals: rope_attaches={ropes} blocks_placed={blocks} bots_cyan={} bots_orange={} players_with_known_team={known}", teams[0], teams[1]);
    if failed {
        std::process::exit(1);
    }
}

fn usage(msg: &str) -> ! {
    if !msg.is_empty() {
        eprintln!("error: {msg}");
    }
    eprintln!("usage: shinobi-bot [--addr HOST:PORT] [--count N] [--behaviour circles|strafe|spam|hook|mixed] [--secs S]");
    std::process::exit(if msg.is_empty() { 0 } else { 2 });
}
