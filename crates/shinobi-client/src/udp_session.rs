//! Platform glue for native clients: a non-blocking UDP socket driving `NetClient`, stepped once per
//! 60 Hz sim tick. No render/windowing types, so it lives here and is tested in the fast workspace loop.
//!
//! Clock discipline: ONE clock (`local_tick`, +1.0 per `fixed_step`) feeds both `on_datagram` and
//! `next_packet`. Intent ticks are forced strictly consecutive-or-greater even when a snapshot re-anchors
//! the server-clock estimate, because the server drops duplicate/stale ticks and a gap clears the
//! redundancy window.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, UdpSocket};

use crate::input::{build_intent, Aim, ButtonLatch};
use crate::netclient::{ConnState, NetClient};

/// Ticks of headroom between the estimated server tick and the tick an intent is stamped for.
pub const LEAD_TICKS: u32 = 4;
/// Re-send `hello` this often (ticks) until Welcome arrives. 15 ticks = 250 ms.
pub const HELLO_EVERY_TICKS: u32 = 15;
/// Server -> client datagrams stay under one MTU; leave headroom.
const RX_BUF: usize = 2048;

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionStats {
    pub rx_datagrams: u64,
    pub rx_ignored: u64,
    pub tx_hello: u64,
    pub tx_packets: u64,
    /// Last intent tick sent.
    pub last_intent_tick: Option<u32>,
}

pub struct UdpSession {
    sock: UdpSocket,
    server: SocketAddr,
    pub net: NetClient,
    pub aim: Aim,
    pub buttons: ButtonLatch,
    local_tick: f64,
    hello_timer: u32,
    last_sent: Option<u32>,
    pub stats: SessionStats,
}

impl UdpSession {
    pub fn new(server: SocketAddr) -> io::Result<Self> {
        let bind: SocketAddr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let sock = UdpSocket::bind(bind)?;
        sock.set_nonblocking(true)?;
        Ok(Self {
            sock,
            server,
            net: NetClient::default(),
            aim: Aim::default(),
            buttons: ButtonLatch::default(),
            local_tick: 0.0,
            hello_timer: 0,
            last_sent: None,
            stats: SessionStats::default(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    pub fn local_tick(&self) -> f64 {
        self.local_tick
    }

    pub fn connected(&self) -> bool {
        matches!(self.net.state(), ConnState::Connected { .. })
    }

    /// Entity id in snapshots == player id (server adds the player under the id it welcomes).
    pub fn own_id(&self) -> Option<u16> {
        self.net.player_id()
    }

    /// Interpolation time for remote entities, if the clock is anchored.
    pub fn render_tick(&self) -> Option<f64> {
        self.net.render_tick(self.local_tick)
    }

    fn drain_rx(&mut self) {
        let mut buf = [0u8; RX_BUF];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if from != self.server {
                        self.stats.rx_ignored += 1;
                        continue;
                    }
                    self.stats.rx_datagrams += 1;
                    let _ = self.net.on_datagram(&buf[..n], self.local_tick); // counted in net.bad_datagrams
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue, // ICMP from a dead server
                Err(_) => break,
            }
        }
    }

    /// Call exactly once per fixed 60 Hz tick. `stick` is the view-frame stick (x strafe, z forward).
    pub fn fixed_step(&mut self, stick: (f32, f32)) {
        self.local_tick += 1.0;
        self.drain_rx();

        if !self.connected() {
            if self.hello_timer == 0 {
                if self.sock.send_to(&self.net.hello(), self.server).is_ok() {
                    self.stats.tx_hello += 1;
                }
            }
            self.hello_timer = (self.hello_timer + 1) % HELLO_EVERY_TICKS;
            // a held tap must not vanish while the handshake is pending
            return;
        }

        let Some(est) = self.net.input_tick(self.local_tick, LEAD_TICKS) else { return };
        let tick = match self.last_sent {
            Some(last) => est.max(last.wrapping_add(1)),
            None => est,
        };
        let intent = build_intent(tick, stick, &self.aim, self.buttons.sample());
        if let Some(pkt) = self.net.next_packet(intent, self.local_tick) {
            if self.sock.send_to(&pkt, self.server).is_ok() {
                self.stats.tx_packets += 1;
                self.last_sent = Some(tick);
                self.stats.last_intent_tick = Some(tick);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::datagram::Welcome;
    use shinobi_proto::intent::{Buttons, ClientPacket};
    use std::time::Duration;

    fn fake_server() -> UdpSocket {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        s
    }

    fn settle() {
        std::thread::sleep(Duration::from_millis(15));
    }

    #[test]
    fn hello_repeats_until_welcome_then_intents_are_strictly_increasing() {
        let srv = fake_server();
        let mut c = UdpSession::new(srv.local_addr().unwrap()).unwrap();

        c.fixed_step((0.0, 0.0));
        let mut buf = [0u8; 2048];
        let (n, client_addr) = srv.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], &c.net.hello());
        assert!(!c.connected());

        // client keeps saying hello at HELLO_EVERY_TICKS
        for _ in 0..HELLO_EVERY_TICKS {
            c.fixed_step((0.0, 0.0));
        }
        assert_eq!(c.stats.tx_hello, 2);

        // client_addr is 0.0.0.0:port; reply to the loopback port
        let to: SocketAddr = format!("127.0.0.1:{}", client_addr.port()).parse().unwrap();
        srv.send_to(&Welcome { player_id: 7, server_tick: 1000 }.encode(), to).unwrap();
        settle();
        c.buttons.set(Buttons::SLICE, true);
        c.buttons.set(Buttons::SLICE, false);
        c.fixed_step((0.0, 1.0));
        assert_eq!(c.own_id(), Some(7));

        // drain whatever the server already holds, then look at 12 consecutive steps
        srv.set_read_timeout(Some(Duration::from_millis(30))).unwrap();
        while srv.recv_from(&mut buf).is_ok() {}
        for _ in 0..12 {
            c.fixed_step((0.0, 1.0));
        }
        settle();
        let mut ticks = Vec::new();
        let mut saw_slice_edge_once = 0;
        while let Ok((n, _)) = srv.recv_from(&mut buf) {
            if let Ok(p) = ClientPacket::decode(&buf[..n]) {
                ticks.push(p.intents[0].tick);
                saw_slice_edge_once += p.intents[0].buttons.has(Buttons::SLICE) as u32;
            }
        }
        assert_eq!(ticks.len(), 12, "one packet per fixed step");
        assert!(ticks.windows(2).all(|w| w[1] == w[0] + 1), "consecutive, no dup/gap: {ticks:?}");
        assert!(ticks[0] >= 1000 + LEAD_TICKS, "stamped ahead of the server clock: {ticks:?}");
        assert_eq!(saw_slice_edge_once, 0, "slice edge was consumed by the connect step, not repeated");
    }

    #[test]
    fn ignores_datagrams_from_other_hosts() {
        let srv = fake_server();
        let mut c = UdpSession::new(srv.local_addr().unwrap()).unwrap();
        c.fixed_step((0.0, 0.0));
        let mut buf = [0u8; 64];
        let (_, client_addr) = srv.recv_from(&mut buf).unwrap();
        let to: SocketAddr = format!("127.0.0.1:{}", client_addr.port()).parse().unwrap();
        let imposter = UdpSocket::bind("127.0.0.1:0").unwrap();
        imposter.send_to(&Welcome { player_id: 9, server_tick: 5 }.encode(), to).unwrap();
        settle();
        c.fixed_step((0.0, 0.0));
        assert!(!c.connected());
        assert_eq!(c.stats.rx_ignored, 1);
    }
}
