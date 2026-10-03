//! UDP lag proxy for testing over a bad network: sits between a game client and the server and adds one-way delay,
//! jitter (which also reorders), random loss, extra reordering and an optional bandwidth cap, in both directions.
//!
//!   shinobi-lagproxy --listen 127.0.0.1:7778 --to 127.0.0.1:7777 --delay-ms 50 --jitter-ms 40 --loss 5
//!
//! Point the game at the proxy (`--server 127.0.0.1:7778`) instead of the server. Every client address gets its own
//! upstream socket, so several players can share one proxy; idle sessions are dropped after 30 s. The impairments
//! apply per direction, so `--delay-ms 50` is a 100 ms round trip. No dependencies: a tiny xorshift RNG
//! (`--seed N` makes the loss / jitter pattern repeatable).
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct ProxyCfg {
    pub listen: SocketAddr,
    pub to: SocketAddr,
    /// One-way base delay, applied to each direction.
    pub delay_ms: f64,
    /// Each packet's delay is `delay_ms` + uniform(-jitter, +jitter), never below 0. Jitter reorders packets.
    pub jitter_ms: f64,
    /// Percent of packets dropped, per direction.
    pub loss_pct: f64,
    /// Percent of packets that get an extra random delay on top (guaranteed reordering).
    pub reorder_pct: f64,
    /// Bandwidth cap per direction in kbit/s (0 = unlimited). Packets that would queue longer than 250 ms are dropped.
    pub bw_kbps: u32,
    pub seed: u64,
}

impl Default for ProxyCfg {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:7778".parse().unwrap(),
            to: "127.0.0.1:7777".parse().unwrap(),
            delay_ms: 0.0,
            jitter_ms: 0.0,
            loss_pct: 0.0,
            reorder_pct: 0.0,
            bw_kbps: 0,
            seed: 0x5EED,
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One direction of the link: decides whether a packet is dropped and when it is delivered.
pub struct Link {
    cfg: ProxyCfg,
    rng: Rng,
    busy_until: Option<Instant>,
}

/// Longest the bandwidth queue may grow before packets are tail-dropped.
const MAX_QUEUE: Duration = Duration::from_millis(250);

impl Link {
    pub fn new(cfg: ProxyCfg, seed_salt: u64) -> Self {
        Self { cfg, rng: Rng((cfg.seed ^ seed_salt.wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1), busy_until: None }
    }

    /// Delivery time of a `len`-byte packet that arrives at `now`, or `None` if it is lost.
    pub fn plan(&mut self, now: Instant, len: usize) -> Option<Instant> {
        if self.cfg.loss_pct > 0.0 && self.rng.unit() * 100.0 < self.cfg.loss_pct {
            return None;
        }
        let mut leave = now;
        if self.cfg.bw_kbps > 0 {
            let start = self.busy_until.filter(|b| *b > now).unwrap_or(now);
            if start - now > MAX_QUEUE {
                return None;
            }
            let serialize = Duration::from_secs_f64(len as f64 * 8.0 / (self.cfg.bw_kbps as f64 * 1000.0));
            self.busy_until = Some(start + serialize);
            leave = start + serialize;
        }
        let jitter = if self.cfg.jitter_ms > 0.0 { (self.rng.unit() * 2.0 - 1.0) * self.cfg.jitter_ms } else { 0.0 };
        let mut delay = (self.cfg.delay_ms + jitter).max(0.0);
        if self.cfg.reorder_pct > 0.0 && self.rng.unit() * 100.0 < self.cfg.reorder_pct {
            delay += self.rng.unit() * (2.0 * (self.cfg.delay_ms + self.cfg.jitter_ms) + 10.0);
        }
        Some(leave + Duration::from_secs_f64(delay / 1000.0))
    }
}

#[derive(Default)]
pub struct Stats {
    pub up_in: AtomicU64,
    pub up_out: AtomicU64,
    pub up_dropped: AtomicU64,
    pub down_in: AtomicU64,
    pub down_out: AtomicU64,
    pub down_dropped: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub up_in: u64,
    pub up_out: u64,
    pub up_dropped: u64,
    pub down_in: u64,
    pub down_out: u64,
    pub down_dropped: u64,
}

impl Stats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        StatsSnapshot {
            up_in: g(&self.up_in),
            up_out: g(&self.up_out),
            up_dropped: g(&self.up_dropped),
            down_in: g(&self.down_in),
            down_out: g(&self.down_out),
            down_dropped: g(&self.down_dropped),
        }
    }
}

struct Session {
    sock: UdpSocket,
    client: SocketAddr,
    alive: AtomicBool,
}

enum Dest {
    Client(SocketAddr),
    Upstream(Arc<Session>),
}

struct Queued {
    due: Instant,
    seq: u64,
    dest: Dest,
    bytes: Vec<u8>,
}
impl PartialEq for Queued {
    fn eq(&self, o: &Self) -> bool {
        (self.due, self.seq) == (o.due, o.seq)
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Queued {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.due, self.seq).cmp(&(o.due, o.seq))
    }
}

struct Shared {
    heap: Mutex<(BinaryHeap<Reverse<Queued>>, u64)>,
    wake: Condvar,
    stop: AtomicBool,
    up: Mutex<Link>,
    down: Mutex<Link>,
    stats: Stats,
}

impl Shared {
    fn push(&self, due: Instant, dest: Dest, bytes: Vec<u8>) {
        let mut h = self.heap.lock().unwrap();
        h.1 += 1;
        let seq = h.1;
        h.0.push(Reverse(Queued { due, seq, dest, bytes }));
        self.wake.notify_one();
    }
}

/// A running proxy. Dropping it (or `stop`) shuts its threads down.
pub struct Proxy {
    shared: Arc<Shared>,
    local: SocketAddr,
    threads: Vec<JoinHandle<()>>,
}

impl Proxy {
    pub fn start(cfg: ProxyCfg) -> std::io::Result<Proxy> {
        let listen = UdpSocket::bind(cfg.listen)?;
        listen.set_read_timeout(Some(Duration::from_millis(100)))?;
        let local = listen.local_addr()?;
        let shared = Arc::new(Shared {
            heap: Mutex::new((BinaryHeap::new(), 0)),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            up: Mutex::new(Link::new(cfg, 1)),
            down: Mutex::new(Link::new(cfg, 2)),
            stats: Stats::default(),
        });
        let mut threads = Vec::new();
        // delivery thread: sends each queued packet when it is due
        {
            let (sh, send_sock) = (shared.clone(), listen.try_clone()?);
            threads.push(std::thread::spawn(move || deliver(&sh, &send_sock)));
        }
        // receive thread: client -> server direction, and session bookkeeping
        {
            let sh = shared.clone();
            threads.push(std::thread::spawn(move || receive(&sh, listen, cfg.to)));
        }
        Ok(Proxy { shared, local, threads })
    }

    /// The address clients should connect to (useful with `--listen ...:0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.shared.stats.snapshot()
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn deliver(sh: &Shared, listen: &UdpSocket) {
    let mut guard = sh.heap.lock().unwrap();
    while !sh.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        match guard.0.peek().map(|r| r.0.due) {
            Some(due) if due <= now => {
                let Reverse(q) = guard.0.pop().unwrap();
                drop(guard);
                match q.dest {
                    Dest::Client(addr) => {
                        if listen.send_to(&q.bytes, addr).is_ok() {
                            sh.stats.down_out.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Dest::Upstream(s) => {
                        if s.sock.send(&q.bytes).is_ok() {
                            sh.stats.up_out.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                guard = sh.heap.lock().unwrap();
            }
            Some(due) => {
                let (g, _) = sh.wake.wait_timeout(guard, (due - now).min(Duration::from_millis(50))).unwrap();
                guard = g;
            }
            None => {
                let (g, _) = sh.wake.wait_timeout(guard, Duration::from_millis(50)).unwrap();
                guard = g;
            }
        }
    }
}

fn receive(sh: &Arc<Shared>, listen: UdpSocket, to: SocketAddr) {
    let mut sessions: HashMap<SocketAddr, (Arc<Session>, Instant)> = HashMap::new();
    let mut readers: Vec<JoinHandle<()>> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut last_sweep = Instant::now();
    while !sh.stop.load(Ordering::Relaxed) {
        match listen.recv_from(&mut buf) {
            Ok((n, from)) => {
                sh.stats.up_in.fetch_add(1, Ordering::Relaxed);
                let entry = sessions.entry(from).or_insert_with(|| {
                    let sock = UdpSocket::bind("0.0.0.0:0").expect("upstream socket");
                    let _ = sock.connect(to);
                    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
                    let s = Arc::new(Session { sock, client: from, alive: AtomicBool::new(true) });
                    let (sh2, s2) = (sh.clone(), s.clone());
                    readers.push(std::thread::spawn(move || upstream_reader(&sh2, &s2)));
                    (s, Instant::now())
                });
                entry.1 = Instant::now();
                let now = Instant::now();
                match sh.up.lock().unwrap().plan(now, n) {
                    Some(due) => sh.push(due, Dest::Upstream(entry.0.clone()), buf[..n].to_vec()),
                    None => {
                        sh.stats.up_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => {}
        }
        if last_sweep.elapsed() > Duration::from_secs(1) {
            last_sweep = Instant::now();
            sessions.retain(|_, (s, seen)| {
                let keep = seen.elapsed() < Duration::from_secs(30);
                if !keep {
                    s.alive.store(false, Ordering::Relaxed);
                }
                keep
            });
        }
    }
    for (s, _) in sessions.values() {
        s.alive.store(false, Ordering::Relaxed);
    }
    for r in readers {
        let _ = r.join();
    }
}

fn upstream_reader(sh: &Shared, s: &Session) {
    let mut buf = [0u8; 4096];
    while !sh.stop.load(Ordering::Relaxed) && s.alive.load(Ordering::Relaxed) {
        match s.sock.recv(&mut buf) {
            Ok(n) => {
                sh.stats.down_in.fetch_add(1, Ordering::Relaxed);
                match sh.down.lock().unwrap().plan(Instant::now(), n) {
                    Some(due) => sh.push(due, Dest::Client(s.client), buf[..n].to_vec()),
                    None => {
                        sh.stats.down_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)), // e.g. ICMP refused while the server restarts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(f: impl FnOnce(&mut ProxyCfg)) -> ProxyCfg {
        let mut c = ProxyCfg { listen: "127.0.0.1:0".parse().unwrap(), ..Default::default() };
        f(&mut c);
        c
    }

    #[test]
    fn link_loss_rate_delay_range_and_reordering_follow_the_config() {
        let mut l = Link::new(cfg(|c| {
            c.delay_ms = 50.0;
            c.jitter_ms = 40.0;
            c.loss_pct = 5.0;
        }), 1);
        let t0 = Instant::now();
        let (mut lost, mut min, mut max, mut inversions, mut prev) = (0, f64::MAX, 0f64, 0, None::<Instant>);
        for i in 0..20_000u32 {
            let now = t0 + Duration::from_micros(i as u64 * 500); // 2000 packets/s
            match l.plan(now, 60) {
                None => lost += 1,
                Some(due) => {
                    let d = (due - now).as_secs_f64() * 1000.0;
                    min = min.min(d);
                    max = max.max(d);
                    if prev.map_or(false, |p| due < p) {
                        inversions += 1;
                    }
                    prev = Some(due);
                }
            }
        }
        let loss = lost as f64 / 200.0;
        assert!((3.8..6.2).contains(&loss), "loss {loss}%");
        assert!(min >= 9.9 && max <= 90.1, "delay {min}..{max} ms must stay within 50 +- 40");
        assert!(min < 15.0 && max > 85.0, "the whole jitter range is used: {min}..{max}");
        assert!(inversions > 1000, "40 ms of jitter at 2000 pkt/s reorders a lot: {inversions}");
    }

    #[test]
    fn link_without_impairments_is_a_wire_and_reorder_adds_late_stragglers() {
        let mut l = Link::new(cfg(|_| {}), 1);
        let now = Instant::now();
        assert_eq!(l.plan(now, 100), Some(now));
        let mut r = Link::new(cfg(|c| {
            c.delay_ms = 20.0;
            c.reorder_pct = 20.0;
        }), 1);
        let late = (0..5000).filter(|_| r.plan(now, 100).map_or(false, |d| d - now > Duration::from_millis(21))).count();
        assert!((700..1300).contains(&late), "about 20% get an extra delay: {late}");
    }

    #[test]
    fn bandwidth_cap_spaces_packets_by_serialization_time_and_tail_drops() {
        // 80 kbit/s: a 100-byte packet takes 10 ms
        let mut l = Link::new(cfg(|c| c.bw_kbps = 80), 1);
        let now = Instant::now();
        let a = l.plan(now, 100).unwrap();
        let b = l.plan(now, 100).unwrap();
        assert_eq!(a - now, Duration::from_millis(10));
        assert_eq!(b - a, Duration::from_millis(10), "the second waits for the first");
        // flooding: the queue is capped at 250 ms, everything beyond is dropped
        let mut dropped = 0;
        for _ in 0..100 {
            if l.plan(now, 100).is_none() {
                dropped += 1;
            }
        }
        assert!(dropped > 60, "{dropped} tail drops");
    }

    /// End to end through real sockets: an echo server behind the proxy, 30 ms +- 10 ms each way, 10% loss each way.
    #[test]
    fn proxy_relays_both_ways_with_delay_loss_and_reordering_and_separates_clients() {
        let echo = UdpSocket::bind("127.0.0.1:0").unwrap();
        echo.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        let echo_addr = echo.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let echo_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while !stop2.load(Ordering::Relaxed) {
                if let Ok((n, from)) = echo.recv_from(&mut buf) {
                    let _ = echo.send_to(&buf[..n], from);
                }
            }
        });
        let proxy = Proxy::start(cfg(|c| {
            c.to = echo_addr;
            c.delay_ms = 30.0;
            c.jitter_ms = 10.0;
            c.loss_pct = 10.0;
            c.seed = 7;
        }))
        .unwrap();
        let client = |tag: u8| {
            let s = UdpSocket::bind("127.0.0.1:0").unwrap();
            s.connect(proxy.local_addr()).unwrap();
            s.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
            (s, tag)
        };
        let (a, ta) = client(0xA0);
        let (b, tb) = client(0xB0);
        let t0 = Instant::now();
        let mut sent_at = HashMap::new();
        let (mut got, mut rtts, mut order): (Vec<u16>, Vec<f64>, Vec<u16>) = (vec![], vec![], vec![]);
        let mut wrong_owner = 0;
        let mut buf = [0u8; 64];
        for i in 0..400u16 {
            for (s, tag) in [(&a, ta), (&b, tb)] {
                let mut p = vec![tag];
                p.extend_from_slice(&i.to_le_bytes());
                s.send(&p).unwrap();
            }
            sent_at.insert(i, Instant::now());
            // poll replies for ~2 ms
            let until = Instant::now() + Duration::from_millis(2);
            while Instant::now() < until {
                for (s, tag) in [(&a, ta), (&b, tb)] {
                    if let Ok(n) = s.recv(&mut buf) {
                        assert_eq!(n, 3);
                        if buf[0] != tag {
                            wrong_owner += 1;
                        } else if tag == ta {
                            let seq = u16::from_le_bytes([buf[1], buf[2]]);
                            got.push(seq);
                            rtts.push(sent_at[&seq].elapsed().as_secs_f64() * 1000.0);
                            order.push(seq);
                        }
                    }
                }
            }
        }
        // collect stragglers
        let until = Instant::now() + Duration::from_millis(250);
        while Instant::now() < until {
            if let Ok(n) = a.recv(&mut buf) {
                assert_eq!(n, 3);
                let seq = u16::from_le_bytes([buf[1], buf[2]]);
                got.push(seq);
                rtts.push(sent_at[&seq].elapsed().as_secs_f64() * 1000.0);
                order.push(seq);
            }
        }
        let _ = t0;
        let st = proxy.stats();
        proxy.stop();
        stop.store(true, Ordering::Relaxed);
        let _ = echo_thread.join();
        assert_eq!(wrong_owner, 0, "each client only gets its own echoes");
        // 10% loss each way: 19% round-trip loss expected
        let loss = 100.0 - got.len() as f64 / 4.0;
        assert!((8.0..32.0).contains(&loss), "round-trip loss {loss}% (stats {st:?})");
        let min = rtts.iter().copied().fold(f64::MAX, f64::min);
        let mean = rtts.iter().sum::<f64>() / rtts.len() as f64;
        assert!(min >= 36.0, "RTT can not beat 2 x (30 - 10) ms: {min}");
        assert!((50.0..75.0).contains(&mean), "mean RTT {mean} ms (2 x 30 ms)");
        let inversions = order.windows(2).filter(|w| w[1] < w[0]).count();
        assert!(inversions > 3, "jitter reorders: {inversions}");
        assert!(st.up_dropped > 10 && st.down_dropped > 10 && st.up_out > 500, "{st:?}");
    }
}
