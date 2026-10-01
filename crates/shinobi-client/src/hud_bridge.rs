//! `RenderView` -> `HudState`: maps what the predicted game and the server report (own hp, cooldowns, hit
//! messages, scoreboard) onto the HUD's transient effects and state. Renderer-agnostic and unit-tested.

use shinobi_proto::gameplay::{PHASE_INTERMISSION, PHASE_LIVE};
use shinobi_sim::match_state::{PhaseKind, ScoreEntry};
use shinobi_sim::pvp::MAX_HP;

use crate::client_game::RenderView;
use crate::hud::{bearing_to, damage_kind, killfeed_tick, score_rows_from_entries, HudFx, HudState, KillEntry, LocalStats, KILLFEED_MAX};
use crate::netclient::NetClient;

#[derive(Default)]
pub struct HudBridge {
    pub fx: HudFx,
    killfeed: Vec<KillEntry>,
    time: f32,
}

impl HudBridge {
    /// Once per rendered frame. `tab_held` shows the scoreboard.
    pub fn update(&mut self, dt: f32, view: &RenderView, net: &NetClient, tab_held: bool) -> HudState {
        self.time += dt;
        self.fx.tick(dt);

        let me = view.local_pos.unwrap_or([0.0; 3]);
        for h in &view.hits {
            if h.taken && h.other == 0 {
                // environmental damage (obstacle / void): nobody to point at, so no direction arrow
            } else if h.taken {
                // bearing to the attacker if we can see them, else straight ahead
                let src = view.remotes.iter().find(|(id, _)| *id == h.other).map(|(_, s)| s.pos);
                self.fx.on_hurt_amount(src.map_or(0.0, |p| bearing_to(me, view.aim.yaw, p)), h.amount);
            } else {
                self.fx.on_hit_amount(damage_kind(h.kind), h.kill, h.amount);
            }
        }

        let local_id = net.player_id().unwrap_or(0);
        // reliable events: match banners, pickup toast, kill feed
        let name = |id: u16| format!("PLAYER {id}");
        for e in &view.events {
            self.fx.on_net_event(e, local_id, name);
            if let Some(k) = KillEntry::from_net(e, local_id, name) {
                self.killfeed.push(k);
                if self.killfeed.len() > KILLFEED_MAX {
                    self.killfeed.remove(0);
                }
            }
        }
        killfeed_tick(&mut self.killfeed, dt);

        let f = &view.fx;
        let scoreboard = view
            .board
            .as_ref()
            .map(|b| {
                let entries: Vec<_> = b
                    .rows
                    .iter()
                    .map(|r| ScoreEntry { id: r.id as u16, kills: r.kills, deaths: r.deaths, alive: r.alive })
                    .collect();
                score_rows_from_entries(&entries, local_id, |id| format!("PLAYER {id}"), |_| 0)
            })
            .unwrap_or_default();
        let stats = LocalStats {
            hp: view.hp.map_or(1.0, |h| h / MAX_HP),
            dash_ready: if f.dash_ready { 1.0 } else { 1.0 - f.slice_cooldown },
            shuriken_ready: 1.0 - f.throw_cooldown,
            slice_ready: 1.0 - f.slice_cooldown,
            // reticle opens while moving fast, airborne, or right after a throw
            spread: ((f.speed / 12.0).clamp(0.0, 1.0) * 0.5 + f.throw_cooldown.clamp(0.0, 1.0) * 0.3 + if f.grounded { 0.0 } else { 0.2 }).clamp(0.0, 1.0),
            ult: 0.0,
            killfeed: self.killfeed.clone(),
            // Tab shows it; the intermission always does (final standings stay up until the next match)
            show_scoreboard: tab_held || view.board.as_ref().map_or(false, |b| b.phase == PHASE_INTERMISSION),
            scoreboard,
            match_clock: view.board.as_ref().map(|b| {
                let kind = match b.phase {
                    PHASE_LIVE => PhaseKind::Live,
                    PHASE_INTERMISSION => PhaseKind::Intermission,
                    _ => PhaseKind::Warmup,
                };
                (kind, b.ticks_left.map(|t| t.div_ceil(shinobi_proto::TICK_HZ as u32)))
            }),
            match_map: view.board.as_ref().map(|b| b.map),
            ..Default::default()
        };
        HudState::from_frame(net, &stats, &self.fx, self.time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::gameplay::{Board, BoardRow, HitMsg, KIND_SLASH};
    use shinobi_sim::combat::DamageKind;

    fn net() -> NetClient {
        NetClient::default()
    }

    fn hit(taken: bool, other: u16) -> HitMsg {
        HitMsg { other, amount: 30, kind: KIND_SLASH, taken, kill: false }
    }

    #[test]
    fn hp_and_cooldowns_map_to_unit_fractions() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hp = Some(MAX_HP * 0.25);
        v.fx.dash_ready = false;
        v.fx.slice_cooldown = 0.4;
        v.fx.throw_cooldown = 1.0;
        let s = b.update(0.016, &v, &net(), false);
        assert!((s.hp - 0.25).abs() < 1e-5);
        assert!((s.dash_ready - 0.6).abs() < 1e-5);
        assert_eq!(s.shuriken_ready, 0.0);
        // before the first extras block everything reads full
        let s = HudBridge::default().update(0.016, &RenderView::default(), &net(), false);
        assert_eq!((s.hp, s.dash_ready, s.shuriken_ready), (1.0, 0.0 + 1.0, 1.0));
    }

    #[test]
    fn dealt_hit_makes_a_marker_that_expires() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hits.push(hit(false, 7));
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.since_hit.is_some() && s.hit_kind == Some(DamageKind::Slash));
        v.hits.clear();
        let s = b.update(1.0, &v, &net(), false);
        assert!(s.since_hit.is_none(), "marker gone after its short life");
    }

    #[test]
    fn taken_hit_points_at_the_attacker() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.local_pos = Some([0.0, 0.8, 0.0]);
        v.aim.yaw = 0.0; // facing -Z
        v.remotes.push((9, crate::interp::Sample { pos: [10.0, 0.8, 0.0], yaw: 0.0, extrapolated: false })); // to the right (+X)
        v.hits.push(hit(true, 9));
        let s = b.update(0.016, &v, &net(), false);
        let (_, bearing) = s.since_damage.expect("damage indicator");
        assert!(bearing > 1.0, "attacker on the right => clockwise bearing: {bearing}");
        // unknown attacker: straight ahead
        let mut v2 = RenderView::default();
        v2.hits.push(hit(true, 99));
        let s = HudBridge::default().update(0.016, &v2, &net(), false);
        assert_eq!(s.since_damage.unwrap().1, 0.0);
    }

    #[test]
    fn kill_events_fill_the_feed_and_expire() {
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.events.push(NetEvent::Kill { tick: 5, killer: 2, victim: 3, kind: KIND_SLASH });
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.killfeed.len(), 1);
        assert_eq!((s.killfeed[0].killer.as_str(), s.killfeed[0].victim.as_str()), ("PLAYER 2", "PLAYER 3"));
        v.events.clear();
        let s = b.update(crate::hud::KILLFEED_TIME + 0.1, &v, &net(), false);
        assert!(s.killfeed.is_empty(), "entries expire");
    }

    #[test]
    fn scoreboard_rows_only_while_tab_is_held() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.board = Some(Board { rows: vec![BoardRow { id: 1, kills: 3, deaths: 1, alive: true }, BoardRow { id: 2, kills: 0, deaths: 3, alive: true }], ..Default::default() });
        let s = b.update(0.016, &v, &net(), true);
        assert!(s.show_scoreboard && s.scoreboard.len() == 2);
        assert_eq!(s.scoreboard[0].kills, 3);
        assert!(!b.update(0.016, &v, &net(), false).show_scoreboard);
    }

    #[test]
    fn intermission_forces_the_final_scoreboard_with_timer_and_match_over_banner() {
        use shinobi_proto::gameplay::PHASE_WARMUP;
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        let board = |phase, ticks| Board { phase, map: 1, ticks_left: ticks, winner: 0, rows: vec![BoardRow { id: 1, kills: 20, deaths: 2, alive: true }] };
        v.board = Some(board(PHASE_LIVE, Some(125 * 60 - 30)));
        let s = b.update(0.016, &v, &net(), false);
        assert!(!s.show_scoreboard, "live: Tab only");
        assert_eq!(s.match_clock, Some((PhaseKind::Live, Some(125))), "ticks round up to whole seconds");
        assert_eq!(s.match_map, Some(1));
        v.board = Some(board(PHASE_WARMUP, None));
        assert_eq!(b.update(0.016, &v, &net(), false).match_clock, Some((PhaseKind::Warmup, None)));
        v.events.push(NetEvent::MatchEnd { tick: 9, winner: 1, reason: 0 });
        v.board = Some(board(PHASE_INTERMISSION, Some(540)));
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.show_scoreboard && s.scoreboard.len() == 1);
        assert_eq!(s.match_clock, Some((PhaseKind::Intermission, Some(9))));
        assert_eq!(s.banner.as_ref().map(|x| x.text.as_str()), Some("MATCH OVER"));
    }

    #[test]
    fn dealt_kill_flows_through_as_a_kill_marker_and_low_hp_pulses() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hp = Some(MAX_HP * 0.1);
        v.hits.push(HitMsg { other: 7, amount: 50, kind: KIND_SLASH, taken: false, kill: true });
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.hit_kill && s.hp < 0.3);
        assert!(crate::hud::Hud::build(&s).iter().any(|p| matches!(p, crate::hud::Prim::Ring { .. })), "kill ring drawn");
    }

    #[test]
    fn hazard_damage_shows_no_direction_arrow_but_a_weapon_hit_does() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hits.push(HitMsg { other: 0, amount: 20, kind: 3 /* environmental */, taken: true, kill: false });
        assert!(b.update(0.016, &v, &net(), false).since_damage.is_none());
        v.hits.clear();
        v.hits.push(hit(true, 5));
        assert!(b.update(0.016, &v, &net(), false).since_damage.is_some());
    }

    #[test]
    fn katana_cooldown_and_reticle_spread_come_from_local_fx() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.fx.grounded = true;
        v.fx.slice_cooldown = 0.25;
        let still = b.update(0.016, &v, &net(), false);
        assert!((still.slice_ready - 0.75).abs() < 1e-5 && still.spread == 0.0);
        v.fx.speed = 12.0;
        v.fx.grounded = false;
        v.fx.throw_cooldown = 1.0;
        let busy = b.update(0.016, &v, &net(), false);
        assert_eq!(busy.spread, 1.0);
    }
}
