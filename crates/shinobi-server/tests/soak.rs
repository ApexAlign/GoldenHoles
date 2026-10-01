//! Soak: the real server binary vs 16 scripted bots (full player cap) over loopback UDP.
//!
//! Gate: `cargo test --release -p shinobi-server --test soak` runs 60 s with the tick-rate gate HARD.
//! Dev profile skips unless `SHINOBI_SOAK_SECS=N` is set (then tick rate is only a warning: the dev sim is
//! too slow to hold 60 Hz with 16 players). `step_avg_ms` above 50% of the tick budget always warns, never fails.
//!
//! Asserts: every bot connects, zero undecodable datagrams, nobody kicked (client view AND server log),
//! server tick rate within 1% of 60 Hz (server's own stats windows + bots' snapshot-tick measurement),
//! and that snapshots flowed (no light walls / obstacle waves are expected by default).

use std::io::{BufRead, BufReader};
use std::net::UdpSocket;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shinobi_bot::{run, Behaviour, BotConfig};

const BOTS: u32 = 16;

fn hz_of(line: &str) -> Option<f64> {
    line.split_whitespace().find_map(|w| w.strip_prefix("hz=")).and_then(|v| v.parse().ok())
}

#[test]
fn sixteen_bots_soak_clean() {
    let release = !cfg!(debug_assertions);
    let opt_in: Option<f64> = std::env::var("SHINOBI_SOAK_SECS").ok().and_then(|v| v.parse().ok());
    let Some(secs) = opt_in.or(release.then_some(60.0)) else {
        eprintln!("soak skipped (dev profile): run with --release, or set SHINOBI_SOAK_SECS=N");
        return;
    };
    let stats_secs = (secs / 3.0).clamp(1.0, 10.0);

    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_shinobi-server"))
        .arg(&addr)
        .env("SHINOBI_STATS_SECS", stats_secs.to_string())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn server");
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let reader = {
        let (log, err) = (log.clone(), child.stderr.take().unwrap());
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                log.lock().unwrap().push(line);
            }
        })
    };
    std::thread::sleep(Duration::from_millis(300));

    let stop = Arc::new(AtomicBool::new(false));
    let handles: Vec<_> = (0..BOTS)
        .map(|i| {
            let (stop, cfg) = (stop.clone(), BotConfig::new(&addr, Behaviour::Mixed, i));
            let h = std::thread::spawn(move || run(&cfg, &stop));
            std::thread::sleep(Duration::from_millis(30));
            h
        })
        .collect();
    std::thread::sleep(Duration::from_secs_f64(secs));
    stop.store(true, Ordering::Relaxed);
    let reports: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    let log = log.lock().unwrap().clone();
    for l in log.iter().filter(|l| !l.starts_with("join ")).take(40) {
        eprintln!("{l}");
    }

    // 1. everyone joined, distinct slots
    assert!(reports.iter().all(|r| !r.connect_failed), "a bot failed to connect: {reports:#?}");
    let mut ids: Vec<_> = reports.iter().map(|r| r.player_id.unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), BOTS as usize);

    // 2. zero bad datagrams
    let bad: u32 = reports.iter().map(|r| r.bad_datagrams).sum();
    assert_eq!(bad, 0, "undecodable datagrams");

    // 3. no false kicks, from either side
    let kicked: Vec<_> = reports.iter().filter_map(|r| r.kicked.map(|k| (r.index, k))).collect();
    assert!(kicked.is_empty(), "bots were kicked: {kicked:?}");
    let bad_log: Vec<_> = log.iter().filter(|l| l.starts_with("kick ") || l.starts_with("timeout ")).collect();
    assert!(bad_log.is_empty(), "server kicked/timed out a bot: {bad_log:?}");

    // 4. tick rate within 1% (hard on release; warning on dev, where the sim cannot keep up)
    let windows: Vec<f64> = log.iter().filter(|l| l.starts_with("stats:")).filter_map(|l| hz_of(l)).collect();
    assert!(!windows.is_empty(), "server printed no stats line");
    let bot_hz: Vec<f64> = reports.iter().filter_map(|r| r.server_hz()).collect();
    assert_eq!(bot_hz.len(), BOTS as usize);
    let mean = bot_hz.iter().sum::<f64>() / bot_hz.len() as f64;
    let rate_ok = windows.iter().all(|hz| (59.4..=60.6).contains(hz))
        && (59.4..=60.6).contains(&mean)
        && bot_hz.iter().all(|h| (58.8..=61.2).contains(h));
    if release {
        assert!(rate_ok, "tick rate outside 1% of 60 Hz: server windows {windows:?}, bots mean {mean:.2}, bots {bot_hz:?}");
    } else if !rate_ok {
        eprintln!("WARN (dev profile): tick rate outside 1%: server windows {windows:?}, bots mean {mean:.2}");
    }
    // soft target: sim step average under 50% of the 16.7 ms budget
    let budget_half = 0.5 * 1000.0 / 60.0;
    for l in log.iter().filter(|l| l.starts_with("stats:")) {
        let avg = l.split_whitespace().find_map(|w| w.strip_prefix("step_avg_ms=")).and_then(|v| v.parse::<f64>().ok());
        if let Some(a) = avg.filter(|a| *a > budget_half) {
            eprintln!("WARN: step_avg_ms {a:.2} > {budget_half:.1} (50% of tick budget): {l}");
        }
    }

    // 5. data actually flowed
    for r in &reports {
        assert!(r.snapshots as f64 > secs * 20.0, "bot {} got only {} snapshots", r.index, r.snapshots);
        assert_eq!((r.wall_events, r.obstacle_events), (0, 0), "bot {}: shooter has no light walls or obstacle waves by default", r.index);
        assert!(r.packets_sent as f64 > secs * 50.0, "bot {} sent only {} packets", r.index, r.packets_sent);
    }
}
