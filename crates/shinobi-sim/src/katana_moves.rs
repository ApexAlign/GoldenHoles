//! Server-side bookkeeping of the katana moves (the weapon half lives in `combat/katana.rs`):
//!
//! * **Counter cut**: a parry opens a short window in which the parrier's next cut goes straight for the staggered
//!   attacker, hits for sure and hits harder the harder the parried blow was.
//! * **Whirl cut**: a cut within a few ticks of lowering the shield (or while sliding) is a 360 degree sweep.
//! * **Air cut**: its hits slam the victim into the ground; the landing is a shock-wave that throws bystanders back
//!   (and credits the slammer if one of them goes over the edge).
//! * **Execute**: a blow on a stunned player at or under a fraction of max hp kills (`MeleeCfg::exec_frac`).
//!
//! Everything is a pure function of ticks and positions: the server alone decides, clients only see the results
//! (hit confirms, positions). Every number is a `melee.*` / `combat.katana.*` Balance key; zero turns a move off.

/// A cut this soon after the shield came down is a whirl cut (0.2 s).
pub const WHIRL_UNBLOCK_TICKS: u32 = 12;
/// A slammed victim has this long to land before the slam is forgotten (1.5 s).
pub const SLAM_TICKS: u32 = 90;

/// Per-player state of the moves above.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KatanaMoveState {
    /// A parry landed: (the staggered attacker's id, last tick a counter cut still counts, the parried damage).
    pub counter: Option<(u16, u32, f32)>,
    /// The tick the shield last came down.
    pub unblocked_at: Option<u32>,
    /// The tick of this player's last air cut: its hits that tick slam their victims.
    pub air_cut_tick: Option<u32>,
    /// This player was slammed: (the slammer's id, the tick of the slam, the last tick the landing still counts).
    pub slam: Option<(u16, u32, u32)>,
}

impl KatanaMoveState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The shield was lowered on `tick`.
    pub fn lowered_shield(&mut self, tick: u32) {
        self.unblocked_at = Some(tick);
    }

    /// Is a cut now a whirl cut (the shield came down within `WHIRL_UNBLOCK_TICKS`)?
    pub fn just_unblocked(&self, tick: u32) -> bool {
        self.unblocked_at.map_or(false, |t| tick.wrapping_sub(t) <= WHIRL_UNBLOCK_TICKS)
    }

    /// The counter cut available at `tick`: (the attacker's id, the parried damage).
    pub fn counter_at(&self, tick: u32) -> Option<(u16, f32)> {
        self.counter.filter(|c| tick <= c.1).map(|c| (c.0, c.2))
    }

    /// Does a hit from this player on `tick` slam its victim?
    pub fn slams(&self, tick: u32) -> bool {
        self.air_cut_tick == Some(tick)
    }

    /// A victim was slammed by `by` on `tick`.
    pub fn slammed(&mut self, by: u16, tick: u32) {
        self.slam = Some((by, tick, tick + SLAM_TICKS));
    }

    /// Per tick for a slammed player: `grounded` = on the floor now. Returns the slammer's id exactly once when the
    /// slam lands (the shock-wave goes off), forgets it when it times out.
    pub fn slam_landed(&mut self, grounded: bool, tick: u32) -> Option<u16> {
        let (by, start, until) = self.slam?;
        if tick > until {
            self.slam = None;
            return None;
        }
        if grounded && tick > start {
            self.slam = None;
            return Some(by);
        }
        None
    }
}

/// The velocity a shock-wave centred on `centre` gives a bystander at `pos`: `knock` m/s straight away from the centre
/// along the ground plus a small hop, or `None` when they are out of `radius` (flat) or on another level.
pub fn shock_push(centre: [f32; 3], pos: [f32; 3], radius: f32, knock: f32) -> Option<[f32; 3]> {
    let (dx, dz) = (pos[0] - centre[0], pos[2] - centre[2]);
    let d = (dx * dx + dz * dz).sqrt();
    if radius <= 0.0 || knock <= 0.0 || d > radius || (pos[1] - centre[1]).abs() > 2.5 {
        return None;
    }
    // dead centre: any direction will do, deterministically +x
    let (nx, nz) = if d < 1e-3 { (1.0, 0.0) } else { (dx / d, dz / d) };
    // closer in, harder out
    let k = knock * (1.0 - 0.5 * d / radius);
    Some([nx * k, 3.0, nz * k])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_counter_window_opens_with_a_parry_and_closes_by_itself() {
        let mut s = KatanaMoveState::default();
        assert_eq!(s.counter_at(10), None);
        s.counter = Some((7, 25, 20.0));
        assert_eq!(s.counter_at(25), Some((7, 20.0)), "still inside the window on its last tick");
        assert_eq!(s.counter_at(26), None, "and gone right after");
    }

    #[test]
    fn whirl_follows_the_shield_coming_down_for_a_moment() {
        let mut s = KatanaMoveState::default();
        assert!(!s.just_unblocked(100));
        s.lowered_shield(100);
        assert!(s.just_unblocked(100) && s.just_unblocked(100 + WHIRL_UNBLOCK_TICKS));
        assert!(!s.just_unblocked(101 + WHIRL_UNBLOCK_TICKS));
    }

    #[test]
    fn a_slam_goes_off_when_the_victim_lands_not_before_and_not_late() {
        let mut s = KatanaMoveState::default();
        s.slammed(3, 50);
        assert_eq!(s.slam_landed(true, 50), None, "the slam tick itself is not a landing");
        assert_eq!(s.slam_landed(false, 52), None, "still falling");
        assert_eq!(s.slam_landed(true, 53), Some(3));
        assert_eq!(s.slam_landed(true, 54), None, "only once");
        s.slammed(3, 60);
        assert_eq!(s.slam_landed(true, 60 + SLAM_TICKS + 1), None, "forgotten after the timeout");
        assert!(s.slam.is_none());
    }

    #[test]
    fn the_air_cut_slams_only_on_its_own_tick() {
        let mut s = KatanaMoveState::default();
        assert!(!s.slams(5));
        s.air_cut_tick = Some(5);
        assert!(s.slams(5) && !s.slams(6));
    }

    #[test]
    fn a_shock_wave_throws_bystanders_away_harder_when_close() {
        let c = [0.0, 1.0, 0.0];
        let near = shock_push(c, [1.0, 1.0, 0.0], 3.5, 7.0).unwrap();
        let far = shock_push(c, [3.0, 1.0, 0.0], 3.5, 7.0).unwrap();
        assert!(near[0] > far[0] && far[0] > 0.0 && near[2].abs() < 1e-5, "outward along +x, stronger close in: {near:?} {far:?}");
        assert_eq!(near[1], 3.0, "a little hop");
        assert!(shock_push(c, [4.0, 1.0, 0.0], 3.5, 7.0).is_none(), "outside the radius");
        assert!(shock_push(c, [1.0, 6.0, 0.0], 3.5, 7.0).is_none(), "another level");
        assert!(shock_push(c, [1.0, 1.0, 0.0], 0.0, 7.0).is_none(), "off by default");
        assert!(shock_push(c, [0.0, 1.0, 0.0], 3.5, 7.0).is_some(), "dead centre still pushes");
    }
}
