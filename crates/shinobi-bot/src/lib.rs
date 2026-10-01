//! Headless bot client: the real `NetClient` (sans-IO) driven by a std `UdpSocket` at 60 Hz, playing a scripted
//! behaviour. Used by `shinobi-bot` (CLI) and by the server's soak test. Bots exercise exactly the wire path a
//! real client uses: HELLO/WELCOME, redundant intent packets, acks, delta snapshots, reliable events.

use std::f32::consts::TAU;
use std::io::ErrorKind;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use shinobi_client::input::{build_intent, Aim};
use shinobi_client::netclient::{ConnState, NetClient};
use shinobi_proto::datagram::KickReason;
use shinobi_proto::intent::Buttons;
use shinobi_proto::TICK_HZ;
use shinobi_sim::events::NetEvent;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Behaviour {
    /// Run forward while turning: circles around the spawn, occasional jump.
    Circles,
    /// Zig-zag strafe with WALL_CLING held, jump and dash bursts.
    Strafe,
    /// Circles plus FIRE/SLICE every tick and frequent dash/roll: worst-case input rate.
    Spam,
    /// Cycles the three above by bot index.
    Mixed,
}

impl Behaviour {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "circles" => Some(Self::Circles),
            "strafe" | "wall-run" => Some(Self::Strafe),
            "spam" => Some(Self::Spam),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }

    fn resolve(self, index: u32) -> Self {
        match self {
            Self::Mixed => [Self::Circles, Self::Strafe, Self::Spam][index as usize % 3],
            b => b,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub addr: String,
    pub behaviour: Behaviour,
    pub index: u32,
    /// Ticks of headroom when stamping intents (one-way latency + jitter).
    pub lead_ticks: u32,
}

impl BotConfig {
    pub fn new(addr: impl Into<String>, behaviour: Behaviour, index: u32) -> Self {
        Self { addr: addr.into(), behaviour, index, lead_ticks: 4 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TickWindow {
    pub first_tick: u32,
    pub first_secs: f64,
    pub last_tick: u32,
    pub last_secs: f64,
}

#[derive(Clone, Debug, Default)]
pub struct BotReport {
    pub index: u32,
    pub player_id: Option<u16>,
    pub connect_failed: bool,
    pub kicked: Option<KickReason>,
    pub bad_datagrams: u32,
    pub packets_sent: u64,
    pub datagrams_recv: u64,
    pub snapshots: u64,
    pub wall_events: u64,
    pub obstacle_events: u64,
    pub tick_window: Option<TickWindow>,
}

impl BotReport {
    /// Server tick rate as this bot saw it: snapshot tick delta over local wall time between the first and
    /// newest snapshot. `None` until the window spans at least a second.
    pub fn server_hz(&self) -> Option<f64> {
        let w = self.tick_window?;
        let dt = w.last_secs - w.first_secs;
        (dt >= 1.0).then(|| (w.last_tick.wrapping_sub(w.first_tick)) as f64 / dt)
    }
}

/// One tick of scripted input: (stick, aim, buttons).
fn script(b: Behaviour, index: u32, n: u64, aim: &mut Aim) -> ((f32, f32), Buttons) {
    let secs = n as f32 / TICK_HZ as f32;
    let phase = index as f32 * 0.4;
    match b {
        Behaviour::Circles | Behaviour::Mixed => {
            aim.yaw = (secs * 1.2 + phase).rem_euclid(TAU);
            let jump = n % 90 < 10;
            let held = if jump { Buttons::JUMP_HELD } else { 0 };
            let edge = if n % 90 == 0 { Buttons::JUMP_PRESSED } else { 0 };
            ((0.0, 1.0), Buttons(held | edge))
        }
        Behaviour::Strafe => {
            aim.yaw = phase;
            let x = if (n / 42) % 2 == 0 { 1.0 } else { -1.0 };
            let mut bits = Buttons::WALL_CLING;
            if n % 50 == 0 {
                bits |= Buttons::JUMP_PRESSED | Buttons::JUMP_HELD;
            }
            if n % 120 == 0 {
                bits |= Buttons::DASH;
            }
            ((x, 0.8), Buttons(bits))
        }
        Behaviour::Spam => {
            aim.yaw = (secs * 2.0 + phase).rem_euclid(TAU);
            let mut bits = Buttons::FIRE | Buttons::SLICE;
            if n % 30 == 0 {
                bits |= Buttons::DASH;
            }
            if n % 45 == 7 {
                bits |= Buttons::ROLL;
            }
            ((0.0, 1.0), Buttons(bits))
        }
    }
}

/// Runs one bot until `stop` is set, it is kicked, or the connection fails. Blocking; run it on a thread.
pub fn run(cfg: &BotConfig, stop: &AtomicBool) -> BotReport {
    let mut rep = BotReport { index: cfg.index, ..Default::default() };
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { rep.connect_failed = true; return rep };
    if sock.connect(&cfg.addr).is_err() || sock.set_nonblocking(true).is_err() {
        rep.connect_failed = true;
        return rep;
    }

    let behaviour = cfg.behaviour.resolve(cfg.index);
    let t0 = Instant::now();
    let tick_dur = Duration::from_secs_f64(1.0 / TICK_HZ as f64);
    let mut nc = NetClient::default();
    let mut aim = Aim::default();
    let mut buf = [0u8; 2048];
    let mut next_sim = t0;
    let mut n: u64 = 0;
    let mut newest_seen: Option<u32> = None;

    'main: while !stop.load(Ordering::Relaxed) {
        // ---- receive ----
        loop {
            match sock.recv(&mut buf) {
                Ok(len) => {
                    rep.datagrams_recv += 1;
                    let local = t0.elapsed().as_secs_f64() * TICK_HZ as f64;
                    let _ = nc.on_datagram(&buf[..len], local);
                    if let Some(why) = nc.kicked() {
                        rep.kicked = Some(why);
                        break 'main;
                    }
                    for e in nc.take_events() {
                        match e {
                            NetEvent::WallSpawn(_) => rep.wall_events += 1,
                            NetEvent::ObstacleSpawn(_) => rep.obstacle_events += 1,
                            _ => {}
                        }
                    }
                    if let Some(t) = nc.interp.newest_tick() {
                        if newest_seen != Some(t) {
                            newest_seen = Some(t);
                            rep.snapshots += 1;
                            let secs = t0.elapsed().as_secs_f64();
                            let w = rep.tick_window.get_or_insert(TickWindow {
                                first_tick: t,
                                first_secs: secs,
                                last_tick: t,
                                last_secs: secs,
                            });
                            w.last_tick = t;
                            w.last_secs = secs;
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break 'main,
            }
        }

        let now = Instant::now();
        match nc.state() {
            ConnState::Connecting => {
                // NetClient owns the retransmit policy (HELLO every 100 ms with the newest cookie, give up after 5 s)
                let local = t0.elapsed().as_secs_f64() * TICK_HZ as f64;
                if nc.connect_failed(local) {
                    rep.connect_failed = true;
                    break;
                }
                if let Some(hello) = nc.hello_due(local) {
                    let _ = sock.send(&hello);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            ConnState::Connected { player_id } => {
                rep.player_id = Some(player_id);
                if now < next_sim {
                    std::thread::sleep((next_sim - now).min(Duration::from_millis(2)));
                    continue;
                }
                next_sim += tick_dur;
                if now > next_sim + Duration::from_millis(100) {
                    next_sim = now; // we stalled (scheduler); do not burst-catch-up
                }
                let local = t0.elapsed().as_secs_f64() * TICK_HZ as f64;
                if let Some(tick) = nc.pace_tick(local, cfg.lead_ticks) {
                    let (stick, buttons) = script(behaviour, cfg.index, n, &mut aim);
                    let intent = build_intent(tick, stick, &aim, buttons);
                    if let Some(pkt) = nc.next_packet(intent, local) {
                        if sock.send(&pkt).is_ok() {
                            rep.packets_sent += 1;
                        }
                    }
                }
                n += 1;
            }
        }
    }

    rep.bad_datagrams = nc.bad_datagrams;
    rep
}
