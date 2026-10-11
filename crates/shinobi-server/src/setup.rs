//! What a match world contains: shared level geometry + spawns (`LevelDesc`, also used by the client predictor)
//! plus server-side pickups. One `build` is used at boot, on panic recovery, and (later) on map change, so the
//! three can never disagree.
//!
//! Level file (`--level PATH`), one directive per line, `#` comments:
//! Format and errors are owned by `shinobi_sim::level::parse_level_file`.
//! ```text
//! box    cx cy cz  hx hy hz [rot_z_radians]   # static cuboid (centre, half extents), optional roll for ramps
//! spawn  x y z                                 # capsule-centre spawn point
//! pickup x y z [respawn_secs]                  # grid-beam cube, default respawn 20 s
//! yumi   x y z [respawn_secs]                  # power-weapon pedestal: the digital bow, default respawn 45 s
//! tag    x y z [respawn_secs]                  # power-weapon pedestal: two explosive fuda tags, default respawn 45 s
//! ```

use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::powerup::PowerupKind;
use shinobi_sim::level::{parse_level_file, LevelDesc};
use shinobi_sim::tick::Server;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PickupDef {
    pub id: u32,
    pub pos: [f32; 3],
    pub respawn_secs: f32,
    /// `PowerKind::None` = a data cube; `yumi` / `tag` level lines are power-weapon pedestals.
    pub weapon: shinobi_sim::power::PowerKind,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Setup {
    pub level: LevelDesc,
    pub pickups: Vec<PickupDef>,
    /// Gameplay tuning (hp, respawn, weapon damage, match rules). Default = the demo preset;
    /// `--balance PATH` loads `key = value` lines over the demo preset (`Balance::apply_text`, `Balance::default().to_text()` lists keys).
    pub balance: Balance,
    /// Team of each spawn (parallel to `level.spawns`; `NO_TEAM` = either). From `spawn x y z [team]`.
    pub spawn_teams: Vec<u8>,
    /// `Some(limit)`: team deathmatch to `limit` team kills (set from `--teams` / `--team-size`).
    pub team_frag_limit: Option<u16>,
}

impl Setup {
    /// The built-in arena: `LevelDesc::standard()` plus four cubes around the spawn ring.
    pub fn standard() -> Self {
        let pickups = [[10.0, 1.0, 0.0], [-10.0, 1.0, 0.0], [0.0, 1.0, 10.0], [0.0, 1.0, -10.0]]
            .into_iter()
            .enumerate()
            .map(|(i, pos)| PickupDef { id: i as u32 + 1, pos, respawn_secs: 20.0, weapon: shinobi_sim::power::PowerKind::None })
            .collect();
        Self { level: LevelDesc::standard(), pickups, balance: Balance::demo(), spawn_teams: Vec::new(), team_frag_limit: None }
    }

    /// Level-file text -> `Setup` (format and errors owned by `shinobi_sim::level::parse_level_file`).
    pub fn parse(text: &str) -> Result<Self, String> {
        let f = parse_level_file(text)?;
        let pickups = f
            .pickups
            .iter()
            .enumerate()
            .map(|(i, p)| PickupDef { id: i as u32 + 1, pos: p.pos, respawn_secs: p.respawn_secs, weapon: p.kind })
            .collect();
        Ok(Self { level: f.level, pickups, balance: Balance::demo(), spawn_teams: shinobi_sim::level::parse_spawn_teams(text), team_frag_limit: None })
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Replace the balance with one parsed from a tuning file.
    pub fn with_balance_file(mut self, path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // a tuning file overlays the DEMO preset (the numbers the game is tuned on), so a one-line preset file is enough
        let mut b = Balance::demo();
        b.apply_text(&text).map_err(|errs| format!("{}: {}", path.display(), errs.join("; ")))?;
        // validate against a scratch server now so a bad value fails at boot, not at the first rebuild
        Server::new(Cfg::default()).set_balance(b.clone()).map_err(|e| format!("{}: {}", path.display(), e.join("; ")))?;
        self.balance = b;
        Ok(self)
    }

    /// Fresh `Server` with geometry, spawn points, pickups and balance. No players. The one constructor used at
    /// boot AND on panic recovery (a rebuilt `Server` starts with default balance otherwise).
    pub fn build(&self, cfg: Cfg) -> Server {
        let mut server = Server::new(cfg);
        server.set_balance(self.balance.clone()).expect("balance was validated when the Setup was made");
        self.level.apply_to_server(&mut server);
        server.set_team_spawns(self.level.spawns.clone(), self.spawn_teams.clone());
        if let Some(limit) = self.team_frag_limit {
            server.enable_teams(limit);
        }
        for p in &self.pickups {
            match p.weapon {
                shinobi_sim::power::PowerKind::None => server.add_pickup(p.id, p.pos, PowerupKind::GridBeam, p.respawn_secs),
                w => server.add_weapon_pickup(p.id, p.pos, w, p.respawn_secs),
            }
        }
        server
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_lines_may_carry_a_team_and_reach_the_server() {
        use shinobi_sim::tick::NO_TEAM;
        let s = Setup::parse("box 0 -0.5 0 50 0.5 50\nspawn -20 0.81 0 0\nspawn 20 0.81 0 1\nspawn 0 0.81 9\n").unwrap();
        assert_eq!(s.spawn_teams, vec![0, 1, NO_TEAM]);
        let srv = s.build(Cfg::default());
        assert_eq!((srv.spawns.len(), srv.spawn_teams.clone()), (3, vec![0, 1, NO_TEAM]));
        assert!(Setup::parse("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0 2\n").unwrap_err().contains("team must be 0"));
    }

    #[test]
    fn parse_wraps_the_shared_parser_and_numbers_pickups() {
        let s = Setup::parse("box 0 -0.5 0  50 0.5 50\nspawn 0 0.81 0\npickup 8 1 0\npickup -8 1 0 5\n").unwrap();
        assert_eq!((s.level.boxes.len(), s.level.spawns.len(), s.pickups.len()), (1, 1, 2));
        assert_eq!((s.pickups[0].id, s.pickups[0].respawn_secs, s.pickups[1].id, s.pickups[1].respawn_secs), (1, 20.0, 2, 5.0));
        assert_eq!(s.balance, Balance::demo());
        assert_eq!(s.build(Cfg::default()).pickups.active().count(), 2, "every pickup point is live after build");
        // errors come straight from the shared parser (line numbers included)
        assert!(Setup::parse("box 0 0 0 1 1\nspawn 0 0 0").unwrap_err().contains("line 1"));
    }

    #[test]
    fn shipped_presets_overlay_the_demo_preset_and_set_destruction() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../balance");
        let demo = Balance::demo();
        let d = Setup::standard().with_balance_file(&root.join("demolition.cfg")).unwrap().balance;
        assert_eq!((d.destruct.hp_mult, d.destruct.rez_s, d.destruct.blast_mult, d.destruct.collapse_ticks), (0.5, 0.0, 8.0, 10));
        assert_eq!(d.player, demo.player, "everything the file does not name stays the demo preset");
        assert_eq!(d.combat, demo.combat);
        let f = Setup::standard().with_balance_file(&root.join("fortress.cfg")).unwrap().balance;
        assert_eq!((f.destruct.hp_mult, f.destruct.rez_s, f.destruct.chip_mult), (2.5, 20.0, 0.5));
        let st = Setup::standard().with_balance_file(&root.join("storm.cfg")).unwrap().balance;
        assert!(st.env.quake > 0.5 && st.env.lightning > 0.5, "the storm preset turns both on");
        assert_eq!(st.player, demo.player);
        let mut srv_s = Server::new(Cfg::default());
        srv_s.set_balance(st).unwrap();
        assert!(srv_s.env.cfg.quake && srv_s.env.cfg.lightning);
        // set_balance carries them into the break rules
        let mut srv = Server::new(Cfg::default());
        srv.set_balance(d).unwrap();
        assert_eq!((srv.breaks.cfg.rez_ticks, srv.breaks.cfg.hp_mult, srv.breaks.cfg.collapse_ticks), (0, 0.5, 10));
    }

    #[test]
    fn balance_file_is_validated_and_applied_on_every_build() {
        let dir = std::env::temp_dir().join(format!("shinobi-bal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.txt");
        std::fs::write(&good, Balance::demo().to_text()).unwrap();
        let s = Setup::standard().with_balance_file(&good).unwrap();
        assert_eq!(s.balance, Balance::demo());
        let bad = dir.join("bad.txt");
        std::fs::write(&bad, "this is not a key value line\n").unwrap();
        let e = Setup::standard().with_balance_file(&bad).unwrap_err();
        assert!(e.contains("bad.txt"), "{e}");
        assert!(Setup::standard().with_balance_file(&dir.join("missing.txt")).is_err());
        // build() applies it (twice: boot and recovery must agree)
        let _a = s.build(Cfg::default());
        let _b = s.build(Cfg::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn standard_builds_and_matches_level_desc_standard() {
        let s = Setup::standard();
        assert_eq!(s.level, LevelDesc::standard());
        assert_eq!(s.pickups.len(), 4);
        let server = s.build(Cfg::default());
        assert_eq!(server.world.colliders.len(), s.level.boxes.len());
    }
}
