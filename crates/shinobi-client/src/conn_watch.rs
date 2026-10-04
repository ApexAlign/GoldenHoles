//! Connection status for the first-time user: turns `NetClient` state plus a few timers into the `Screen` to
//! show (connecting / server unreachable / connection lost / kicked), so nobody stares at a blank view.

use crate::netclient::{ConnState, NetClient};
use crate::screens::Screen;

/// Seconds of HELLO retries with no WELCOME before we say the server cannot be reached.
pub const UNREACHABLE_AFTER_S: f32 = 5.0;

#[derive(Clone, Debug, Default)]
pub struct ConnWatch {
    connecting_s: f32,
    silence_s: f32,
    kick_s: f32,
    last_snap: Option<u32>,
}

impl ConnWatch {
    /// Call once per rendered frame. `connect_failed` = `RenderView::connect_failed` (game-level timeout).
    pub fn update(&mut self, dt: f32, net: &NetClient, connect_failed: bool, server: &str) -> Screen {
        match net.state() {
            ConnState::Connecting => self.connecting_s += dt,
            ConnState::Connected { .. } => {
                let newest = net.interp.newest_tick();
                if newest != self.last_snap {
                    self.last_snap = newest;
                    self.silence_s = 0.0;
                } else {
                    self.silence_s += dt;
                }
            }
        }
        if net.kicked().is_some() {
            self.kick_s += dt;
        }
        let s = Screen::for_net(net, self.connecting_s, self.silence_s, self.kick_s);
        match s {
            Screen::Connecting { .. } if connect_failed || self.connecting_s > UNREACHABLE_AFTER_S => {
                Screen::Disconnected { reason: format!("SERVER UNREACHABLE {server}") }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::datagram::{encode_kick, KickReason, Welcome};

    #[test]
    fn connecting_then_unreachable_then_connected_clears() {
        let mut net = NetClient::default();
        let mut w = ConnWatch::default();
        assert!(matches!(w.update(0.1, &net, false, "1.2.3.4:7777"), Screen::Connecting { .. }));
        let s = w.update(UNREACHABLE_AFTER_S, &net, false, "1.2.3.4:7777");
        assert_eq!(s, Screen::Disconnected { reason: "SERVER UNREACHABLE 1.2.3.4:7777".into() });
        // the game's own connect timeout wins immediately
        let mut w2 = ConnWatch::default();
        assert!(matches!(w2.update(0.1, &net, true, "x"), Screen::Disconnected { .. }));
        // a late WELCOME recovers
        net.on_datagram(&Welcome { player_id: 1, server_tick: 10 }.encode(), 0.0).unwrap();
        assert_eq!(w.update(0.1, &net, false, "x"), Screen::InGame);
    }

    #[test]
    fn kick_shows_reason_and_countdown() {
        let mut net = NetClient::default();
        net.on_datagram(&encode_kick(KickReason::Cheat), 0.0).unwrap();
        let mut w = ConnWatch::default();
        match w.update(1.0, &net, false, "x") {
            Screen::Kicked { reason, retry_in_s } => {
                assert_eq!(reason, "CHEAT DETECTION");
                assert!((retry_in_s - (crate::screens::RECONNECT_WAIT_S - 1.0)).abs() < 1e-4, "{retry_in_s}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn silence_after_connect_becomes_connection_lost() {
        let mut net = NetClient::default();
        net.on_datagram(&Welcome { player_id: 1, server_tick: 10 }.encode(), 0.0).unwrap();
        let mut w = ConnWatch::default();
        assert_eq!(w.update(0.5, &net, false, "x"), Screen::InGame);
        let s = w.update(crate::screens::SILENCE_TIMEOUT_S + 0.1, &net, false, "x");
        assert_eq!(s, Screen::Disconnected { reason: "CONNECTION LOST".into() });
    }
}
