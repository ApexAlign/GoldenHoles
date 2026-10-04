//! Native-client glue: a non-blocking UDP socket driving `ClientGame` (prediction + reconciliation + camera).
//! No render/window types, so it is tested in the fast workspace loop. The shell (Bevy) calls
//! `fixed_step` exactly once per 60 Hz tick and `view` once per rendered frame.

use std::io::{self, ErrorKind};
use std::net::{SocketAddr, UdpSocket};

use shinobi_sim::level::LevelDesc;

use crate::client_game::{ClientGame, ClientGameCfg, InputFrame, OwnAck, RenderView};
use crate::udp_session::SessionStats;

const RX_BUF: usize = 2048;

pub struct UdpGame {
    sock: UdpSocket,
    server: SocketAddr,
    pub game: ClientGame,
    local_tick: f64,
    t0: std::time::Instant,
    pub stats: SessionStats,
}

impl UdpGame {
    pub fn new(server: SocketAddr, level: LevelDesc, cfg: ClientGameCfg) -> io::Result<Self> {
        let bind: SocketAddr = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let sock = UdpSocket::bind(bind)?;
        sock.set_nonblocking(true)?;
        Ok(Self { sock, server, game: ClientGame::new(level, cfg), local_tick: 0.0, t0: std::time::Instant::now(), stats: SessionStats::default() })
    }

    /// Drop this session and start a fresh one against the same server: new socket (so a new peer address and a new
    /// HELLO / cookie exchange), new `ClientGame` (prediction, interpolation and kick state all reset). Used by the
    /// shell's "press Enter to reconnect" after a kick or a lost connection.
    pub fn reconnect(&mut self, level: LevelDesc, cfg: ClientGameCfg) -> io::Result<()> {
        *self = UdpGame::new(self.server, level, cfg)?;
        Ok(())
    }

    /// Send arbitrary bytes to the server from this session's socket (tests: provoke a flood kick).
    pub fn send_raw(&self, bytes: &[u8]) -> io::Result<usize> {
        self.sock.send_to(bytes, self.server)
    }

    pub fn local_tick(&self) -> f64 {
        self.local_tick
    }

    /// Real time since the session started, in 60 Hz ticks: the server clock estimate follows this, not the step count.
    fn sync_wall(&mut self, local: f64) {
        let wall = self.t0.elapsed().as_secs_f64() * shinobi_proto::TICK_HZ as f64;
        self.game.net.sync_wall(local, wall);
    }

    pub fn connected(&self) -> bool {
        self.game.net.player_id().is_some()
    }

    pub fn own_id(&self) -> Option<u16> {
        self.game.net.player_id()
    }

    /// Advance the tick clock without touching the socket (offline/regression scenes feed snapshots themselves).
    pub fn advance_clock(&mut self) {
        self.local_tick += 1.0;
    }

    /// One clock for datagrams and intents: `local_tick` advances by exactly 1.0 per call.
    pub fn fixed_step(&mut self, frame: &InputFrame) {
        self.local_tick += 1.0;
        self.sync_wall(self.local_tick);
        self.drain_rx();
        // owner-only state from the newest snapshot feeds reconciliation
        if let Some(u) = self.game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                self.game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }
        if let Some(pkt) = self.game.sim_tick(frame, self.local_tick) {
            if self.sock.send_to(&pkt, self.server).is_ok() {
                if self.connected() {
                    self.stats.tx_packets += 1;
                } else {
                    self.stats.tx_hello += 1;
                }
            }
        }
    }

    pub fn view(&mut self) -> RenderView {
        self.sync_wall(self.local_tick);
        self.game.view(self.local_tick)
    }

    /// Like `view`, but `frac` (0..1, how far the render frame is into the current fixed step) lets the own
    /// position and the remote players glide between 60 Hz ticks instead of stepping.
    /// Draw at an explicit client time in ticks (`DisplayClock` output).
    pub fn view_at(&mut self, t: f64) -> RenderView {
        self.game.view(t)
    }

    pub fn view_frac(&mut self, frac: f64) -> RenderView {
        self.sync_wall(self.local_tick + frac.clamp(0.0, 1.0));
        self.game.view(self.local_tick + frac.clamp(0.0, 1.0))
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
                    let _ = self.game.on_datagram(&buf[..n], self.local_tick); // failures counted in net.bad_datagrams
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::datagram::Welcome;
    use shinobi_proto::intent::ClientPacket;
    use std::time::Duration;

    #[test]
    fn handshake_then_consecutive_intents_and_a_view() {
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        srv.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut c = UdpGame::new(srv.local_addr().unwrap(), LevelDesc::standard(), ClientGameCfg::default()).unwrap();

        c.fixed_step(&InputFrame::default());
        let mut buf = [0u8; 2048];
        let (n, caddr) = srv.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], &c.game.net.hello());
        assert!(!c.connected() && !c.view().connected);

        let to: SocketAddr = format!("127.0.0.1:{}", caddr.port()).parse().unwrap();
        srv.send_to(&Welcome { player_id: 3, server_tick: 500 }.encode(), to).unwrap();
        std::thread::sleep(Duration::from_millis(15));
        c.fixed_step(&InputFrame::default());
        assert_eq!(c.own_id(), Some(3));
        assert!(c.view().connected);

        srv.set_read_timeout(Some(Duration::from_millis(30))).unwrap();
        while srv.recv_from(&mut buf).is_ok() {}
        for _ in 0..10 {
            // paced like the real 60 Hz loop: a burst of steps with no wall time between them is held, not stamped ahead
            std::thread::sleep(Duration::from_millis(16));
            c.fixed_step(&InputFrame { stick: (0.0, 1.0), ..Default::default() });
        }
        std::thread::sleep(Duration::from_millis(15));
        let mut ticks = Vec::new();
        while let Ok((n, _)) = srv.recv_from(&mut buf) {
            if let Ok(p) = ClientPacket::decode(&buf[..n]) {
                ticks.push(p.intents[0].tick);
            }
        }
        assert_eq!(ticks.len(), 10);
        assert!(ticks.windows(2).all(|w| w[1] == w[0] + 1), "consecutive: {ticks:?}");
        assert!(ticks[0] >= 500, "stamped at/after the server clock: {ticks:?}");
    }

    #[test]
    fn reconnect_after_a_kick_rejoins_from_a_new_address_with_a_clean_session() {
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        srv.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut c = UdpGame::new(srv.local_addr().unwrap(), LevelDesc::standard(), ClientGameCfg::default()).unwrap();
        c.fixed_step(&InputFrame::default());
        let mut buf = [0u8; 2048];
        let (_, first) = srv.recv_from(&mut buf).unwrap();
        let to: SocketAddr = format!("127.0.0.1:{}", first.port()).parse().unwrap();
        srv.send_to(&Welcome { player_id: 3, server_tick: 100 }.encode(), to).unwrap();
        srv.send_to(&shinobi_proto::datagram::encode_kick(shinobi_proto::datagram::KickReason::Flood), to).unwrap();
        std::thread::sleep(Duration::from_millis(15));
        c.fixed_step(&InputFrame::default());
        assert!(c.game.net.kicked().is_some(), "kicked");

        c.reconnect(LevelDesc::standard(), ClientGameCfg::default()).unwrap();
        assert!(c.game.net.kicked().is_none() && !c.connected(), "clean session");
        c.fixed_step(&InputFrame::default());
        let (n, second) = srv.recv_from(&mut buf).unwrap();
        assert_ne!(second.port(), first.port(), "a new socket, a new peer address");
        assert_eq!(&buf[..n], &c.game.net.hello());
        let to2: SocketAddr = format!("127.0.0.1:{}", second.port()).parse().unwrap();
        srv.send_to(&Welcome { player_id: 9, server_tick: 900 }.encode(), to2).unwrap();
        std::thread::sleep(Duration::from_millis(15));
        c.fixed_step(&InputFrame::default());
        assert_eq!(c.own_id(), Some(9));
        assert!(c.view().connected);
    }
}
