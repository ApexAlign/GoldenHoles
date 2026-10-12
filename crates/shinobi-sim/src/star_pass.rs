//! Server-side world effects of the flash star (docs/stars.md). The launcher (`combat/shuriken.rs`) flies the star;
//! everything that touches other players happens here, once per tick after every player has moved:
//!
//! * FLASH bursts white out (and daze) everyone facing them (`flash_strength`), no line of sight needed; the thrower only
//!   when looking at it from close by.

use rapier3d::prelude::*;
use shinobi_proto::quant::{dq_pitch, dq_yaw};

use crate::combat::stars::{flash_strength, view_dir};
use crate::combat::{solid_groups, CombatEvent, GROUP_PROJECTILE};
use crate::match_state::TICK_HZ;
use crate::pvp::PvpHit;
use crate::tick::{allied, Player, Server};

fn dist3(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

impl Server {
    /// Clear line (no solid geometry) between two points.
    pub(crate) fn los_clear(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        let d = vector![b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = d.norm();
        if len < 1e-3 {
            return true;
        }
        let ray = Ray::new(Point::from(vector![a[0], a[1], a[2]]), d / len);
        let pass = |h: ColliderHandle, c: &Collider| crate::combat::passes_projectile(h, c);
        let filter = QueryFilter::default().predicate(&pass).groups(InteractionGroups::new(GROUP_PROJECTILE, solid_groups()));
        self.world.queries.cast_ray(&self.world.bodies, &self.world.colliders, &ray, (len - 0.05).max(0.0), true, filter).is_none()
    }

    /// Flash bursts for this tick. `ev_start` = index in `self.events` where this tick's combat events
    /// begin; the resulting hits are appended to `hits` (settled by `apply_hits` with everyone else's).
    pub(crate) fn star_pass(&mut self, tick: u32, ev_start: usize, _hits: &mut Vec<PvpHit>) {
        let sc = self.balance.combat.star;
        let new_events: Vec<(u16, CombatEvent)> = self.events[ev_start.min(self.events.len())..].to_vec();
        let idx_of = |players: &[Player], id: u16| players.iter().position(|p| p.id == id);
        let mut out_events: Vec<(u16, CombatEvent)> = Vec::new();

        // ---- flash: white-out and daze everyone who faces the burst (no line of sight needed) ----
        for (owner, e) in &new_events {
            let CombatEvent::StarFlash { pos } = *e else { continue };
            let burst = [pos.x, pos.y, pos.z];
            let Some(oi) = idx_of(&self.players, *owner) else { continue };
            let oteam = self.players[oi].team;
            for qi in 0..self.players.len() {
                let q = &self.players[qi];
                if (qi != oi && allied(oteam, q.team)) || !q.vitals.alive() {
                    continue;
                }
                let at = q.pos(&self.world);
                let eye = [at[0], at[1] + q.ch.eye_above(), at[2]];
                // the thrower is only caught looking at it from close by
                if qi == oi && dist3(eye, burst) > sc.flash_self_radius {
                    continue;
                }
                let s = flash_strength(&sc, eye, view_dir(dq_yaw(q.yaw), dq_pitch(q.pitch)), burst);
                if s <= 0.0 {
                    continue;
                }
                let ticks = ((sc.flash_secs * (0.4 + 0.6 * s) * TICK_HZ as f32) as u32).min(u16::MAX as u32);
                let until = tick + ticks;
                let q = &mut self.players[qi];
                q.blind_until = q.blind_until.max(until);
                out_events.push((*owner, CombatEvent::StarBlind { victim: q.id, strength_q: (s * 255.0).round() as u8, ticks: ticks as u16 }));
            }
        }

        self.events.extend(out_events);
    }
}
