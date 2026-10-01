//! Global attack-ticket queue.
//!
//! In first person, a katana lunge from outside the camera frustum is unreadable and feels unfair. A bot that is
//! *off screen* for its target must hold a ticket before it may start a dash-slice; only `max_active` tickets
//! exist at once (and at most `per_target` per human). Everyone else zones at range until a ticket frees up.
//! Bots already inside the target's frustum do not need one: the player can see them coming.
//!
//! Settled once per tick by the director, before any brain thinks, in a fixed order (longest-waiting first,
//! then nearest, then lowest bot id) so no iteration order decides who strikes: same pattern as
//! `pvp::apply_hits`. No hash maps; everything is small sorted vectors.

#[derive(Clone, Copy, Debug)]
pub struct TicketCfg {
    pub max_active: usize,
    pub per_target: usize,
    /// A ticket not used within this many ticks is revoked (bot stuck, lost the target, etc.).
    pub ttl_ticks: u32,
    /// After a strike the holder cannot re-request for this long: stops one bot monopolising the queue.
    pub cooldown_ticks: u32,
}

impl Default for TicketCfg {
    fn default() -> Self {
        Self { max_active: 2, per_target: 1, ttl_ticks: 120, cooldown_ticks: 90 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub bot: u16,
    pub target: u16,
    pub granted: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub bot: u16,
    pub target: u16,
    /// Tick the bot first asked (older = served first).
    pub since: u32,
    pub dist: f32,
}

#[derive(Default)]
pub struct AttackTickets {
    pub cfg: TicketCfg,
    active: Vec<Ticket>,
    /// (bot, tick it may ask again)
    cooling: Vec<(u16, u32)>,
    /// Total grants, for tests / stats.
    pub granted_total: u64,
}

impl AttackTickets {
    pub fn new(cfg: TicketCfg) -> Self {
        Self { cfg, ..Default::default() }
    }

    pub fn holds(&self, bot: u16) -> bool {
        self.active.iter().any(|t| t.bot == bot)
    }

    pub fn active(&self) -> &[Ticket] {
        &self.active
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// The holder's strike is over (or aborted).
    pub fn release(&mut self, bot: u16, tick: u32) {
        if let Some(i) = self.active.iter().position(|t| t.bot == bot) {
            self.active.remove(i);
            self.cool(bot, tick);
        }
    }

    fn cool(&mut self, bot: u16, tick: u32) {
        self.cooling.retain(|c| c.0 != bot);
        self.cooling.push((bot, tick.saturating_add(self.cfg.cooldown_ticks)));
    }

    pub fn cooling_down(&self, bot: u16, tick: u32) -> bool {
        self.cooling.iter().any(|c| c.0 == bot && tick < c.1)
    }

    /// Expire stale tickets, then hand out free ones. `alive` says whether a player id is still playing.
    pub fn settle(&mut self, tick: u32, requests: &[Request], alive: impl Fn(u16) -> bool) {
        let ttl = self.cfg.ttl_ticks;
        let mut dropped = Vec::new();
        self.active.retain(|t| {
            let keep = tick < t.granted.saturating_add(ttl) && alive(t.bot) && alive(t.target);
            if !keep {
                dropped.push(t.bot);
            }
            keep
        });
        for b in dropped {
            self.cool(b, tick);
        }
        self.cooling.retain(|c| tick < c.1);

        let mut order: Vec<&Request> = requests
            .iter()
            .filter(|r| !self.holds(r.bot) && !self.cooling_down(r.bot, tick) && alive(r.bot) && alive(r.target))
            .collect();
        order.sort_by(|a, b| {
            a.since
                .cmp(&b.since)
                .then(a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.bot.cmp(&b.bot))
        });
        for r in order {
            if self.active.len() >= self.cfg.max_active {
                break;
            }
            let on_target = self.active.iter().filter(|t| t.target == r.target).count();
            if on_target >= self.cfg.per_target {
                continue;
            }
            self.active.push(Ticket { bot: r.bot, target: r.target, granted: tick });
            self.granted_total += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(bot: u16, target: u16, since: u32, dist: f32) -> Request {
        Request { bot, target, since, dist }
    }

    #[test]
    fn never_exceeds_global_or_per_target_cap() {
        let mut q = AttackTickets::new(TicketCfg { max_active: 2, per_target: 1, ..Default::default() });
        let reqs: Vec<_> = (201..=208).map(|b| req(b, if b % 2 == 0 { 1 } else { 2 }, 0, 5.0)).collect();
        q.settle(10, &reqs, |_| true);
        assert_eq!(q.active_count(), 2);
        assert_ne!(q.active()[0].target, q.active()[1].target, "per-target cap of 1 spreads tickets");
        // same tick, same inputs: same winners
        let mut q2 = AttackTickets::new(q.cfg);
        q2.settle(10, &reqs, |_| true);
        assert_eq!(q.active(), q2.active());
    }

    #[test]
    fn longest_waiting_then_nearest_then_id() {
        let mut q = AttackTickets::new(TicketCfg { max_active: 1, per_target: 5, ..Default::default() });
        q.settle(5, &[req(203, 1, 4, 9.0), req(201, 1, 4, 9.0), req(202, 1, 3, 20.0)], |_| true);
        assert_eq!(q.active()[0].bot, 202, "waited longest");
        let mut q = AttackTickets::new(TicketCfg { max_active: 1, per_target: 5, ..Default::default() });
        q.settle(5, &[req(203, 1, 4, 9.0), req(201, 1, 4, 9.0)], |_| true);
        assert_eq!(q.active()[0].bot, 201, "id breaks the tie");
    }

    #[test]
    fn release_expiry_death_and_cooldown() {
        let mut q = AttackTickets::new(TicketCfg { max_active: 1, per_target: 1, ttl_ticks: 50, cooldown_ticks: 30 });
        q.settle(0, &[req(201, 1, 0, 5.0)], |_| true);
        assert!(q.holds(201));
        q.release(201, 10);
        assert!(!q.holds(201));
        q.settle(11, &[req(201, 1, 0, 5.0)], |_| true);
        assert!(!q.holds(201), "cooling down");
        q.settle(41, &[req(201, 1, 0, 5.0)], |_| true);
        assert!(q.holds(201));
        q.settle(60, &[], |_| true);
        assert!(q.holds(201), "ttl not yet");
        q.settle(91, &[], |_| true);
        assert!(!q.holds(201), "ttl revoked it");
        q.settle(200, &[req(202, 1, 0, 5.0)], |_| true);
        assert!(q.holds(202));
        q.settle(201, &[], |id| id != 1);
        assert_eq!(q.active_count(), 0, "target died");
    }
}
