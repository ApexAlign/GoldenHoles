//! `shinobi-lagproxy --listen 127.0.0.1:7778 --to 127.0.0.1:7777 --delay-ms 50 --jitter-ms 40 --loss 5`
//! See `shinobi_bot::lagproxy` for what each option does. Runs until killed.
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use shinobi_bot::lagproxy::{Proxy, ProxyCfg};

fn usage(msg: &str) -> ! {
    if !msg.is_empty() {
        eprintln!("error: {msg}");
    }
    eprintln!(
        "usage: shinobi-lagproxy --listen ADDR --to ADDR [--delay-ms N] [--jitter-ms N] [--loss PCT] [--reorder PCT] [--bw-kbps N] [--seed N] [--stats-secs N]\n\
         delay / jitter / loss / reorder apply to each direction (delay 50 = 100 ms round trip). Default listen 127.0.0.1:7778, to 127.0.0.1:7777."
    );
    std::process::exit(if msg.is_empty() { 0 } else { 2 });
}

fn addr(s: &str) -> SocketAddr {
    s.to_socket_addrs().ok().and_then(|mut a| a.next()).unwrap_or_else(|| usage(&format!("bad address `{s}`")))
}

fn main() {
    let mut cfg = ProxyCfg::default();
    let mut stats_secs = 10u64;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |f: &str| it.next().cloned().unwrap_or_else(|| usage(&format!("{f} needs a value")));
        let num = |f: &str, v: String| v.parse::<f64>().unwrap_or_else(|_| usage(&format!("{f}: `{v}` is not a number")));
        match a.as_str() {
            "--listen" => cfg.listen = addr(&val("--listen")),
            "--to" => cfg.to = addr(&val("--to")),
            "--delay-ms" => cfg.delay_ms = num("--delay-ms", val("--delay-ms")).max(0.0),
            "--jitter-ms" => cfg.jitter_ms = num("--jitter-ms", val("--jitter-ms")).max(0.0),
            "--loss" => cfg.loss_pct = num("--loss", val("--loss")).clamp(0.0, 100.0),
            "--reorder" => cfg.reorder_pct = num("--reorder", val("--reorder")).clamp(0.0, 100.0),
            "--bw-kbps" => cfg.bw_kbps = num("--bw-kbps", val("--bw-kbps")).max(0.0) as u32,
            "--seed" => cfg.seed = num("--seed", val("--seed")) as u64,
            "--stats-secs" => stats_secs = num("--stats-secs", val("--stats-secs")).max(0.0) as u64,
            "-h" | "--help" => usage(""),
            other => usage(&format!("unknown argument `{other}`")),
        }
    }
    let proxy = Proxy::start(cfg).unwrap_or_else(|e| usage(&format!("cannot listen on {}: {e}", cfg.listen)));
    eprintln!(
        "lagproxy: {} -> {}   one-way delay {} ms +- {} ms, loss {}%, reorder {}%, bandwidth {}",
        proxy.local_addr(),
        cfg.to,
        cfg.delay_ms,
        cfg.jitter_ms,
        cfg.loss_pct,
        cfg.reorder_pct,
        if cfg.bw_kbps == 0 { "unlimited".to_string() } else { format!("{} kbit/s", cfg.bw_kbps) }
    );
    let mut prev = proxy.stats();
    loop {
        std::thread::sleep(Duration::from_secs(if stats_secs == 0 { 3600 } else { stats_secs }));
        if stats_secs == 0 {
            continue;
        }
        let s = proxy.stats();
        eprintln!(
            "lagproxy: up {} in / {} out / {} dropped, down {} in / {} out / {} dropped (last {stats_secs} s)",
            s.up_in - prev.up_in,
            s.up_out - prev.up_out,
            s.up_dropped - prev.up_dropped,
            s.down_in - prev.down_in,
            s.down_out - prev.down_out,
            s.down_dropped - prev.down_dropped
        );
        prev = s;
    }
}
