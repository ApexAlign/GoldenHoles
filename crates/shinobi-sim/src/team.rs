//! Teams (8v8 Tron team shooter, see docs/8v8.md): the one authoritative `Player.team` plus join balancing.
//!
//! Team ids are wire values (WELCOME, scoreboard, snapshot flag): 0 = Cyan, 1 = Orange.
use crate::tick::Server;

pub const TEAM_CYAN: u8 = 0;
pub const TEAM_ORANGE: u8 = 1;
pub const TEAM_COUNT: u8 = 2;

/// `team` normalised to a valid id (anything unknown folds to Cyan, so wire garbage can't index out of range).
pub fn valid(team: u8) -> u8 {
    if team < TEAM_COUNT {
        team
    } else {
        TEAM_CYAN
    }
}

impl Server {
    /// Players on `team`.
    pub fn team_size(&self, team: u8) -> usize {
        self.players.iter().filter(|p| p.team == team).count()
    }

    /// Frags of `team` (sum of its players' kills).
    pub fn team_kills(&self, team: u8) -> u32 {
        self.players.iter().filter(|p| p.team == team).map(|p| p.vitals.kills as u32).sum()
    }

    /// Where a new player should go: the smaller team, then the one with fewer kills (so a joiner helps the side
    /// that is behind), then Cyan.
    pub fn balanced_team(&self) -> u8 {
        let key = |t: u8| (self.team_size(t), self.team_kills(t), t);
        (TEAM_CYAN..TEAM_COUNT).min_by_key(|t| key(*t)).unwrap_or(TEAM_CYAN)
    }

    /// Moves an existing player to `team`. Callers (match flow, bots) decide whether to respawn them.
    pub fn set_team(&mut self, id: u16, team: u8) -> bool {
        match self.players.iter_mut().find(|p| p.id == id) {
            Some(p) => {
                p.team = valid(team);
                true
            }
            None => false,
        }
    }

    /// `add_player` onto a chosen team (`add_player` itself auto-balances).
    pub fn add_player_on(&mut self, id: u16, spawn: [f32; 3], team: u8) {
        self.add_player(id, spawn);
        self.set_team(id, team);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Cfg;

    #[test]
    fn joiners_fill_the_smaller_team_then_the_one_behind() {
        let mut s = Server::new(Cfg::default());
        for id in 1..=5u16 {
            s.add_player(id, [id as f32, 0.81, 0.0]);
        }
        assert_eq!((s.team_size(TEAM_CYAN), s.team_size(TEAM_ORANGE)), (3, 2), "alternates, Cyan first");
        assert_eq!(s.balanced_team(), TEAM_ORANGE, "smaller side");
        // equal sizes: the side with fewer kills gets the joiner
        s.add_player(6, [9.0, 0.81, 0.0]);
        assert_eq!(s.team_size(TEAM_CYAN), s.team_size(TEAM_ORANGE));
        s.players.iter_mut().filter(|p| p.team == TEAM_CYAN).for_each(|p| p.vitals.kills = 2);
        assert_eq!(s.balanced_team(), TEAM_ORANGE, "Orange is behind");
        // leaving rebalances the next join
        s.remove_player(6);
        assert_eq!(s.balanced_team(), TEAM_ORANGE);
    }

    #[test]
    fn set_team_rejects_unknown_players_and_clamps_garbage() {
        let mut s = Server::new(Cfg::default());
        s.add_player(1, [0.0, 0.81, 0.0]);
        assert!(!s.set_team(9, TEAM_ORANGE));
        assert!(s.set_team(1, 200));
        assert_eq!(s.players[0].team, TEAM_CYAN);
    }
}
