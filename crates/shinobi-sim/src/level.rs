//! Static level description shared by the authoritative server and the client predictor, so both build the
//! exact same colliders in the exact same order. Primitive cuboids only (never render meshes).

use crate::character::PhysicsWorld;
use crate::combat::GROUP_WORLD;
use crate::rapier3d::prelude::*;
use crate::tick::Server;

/// An axis-aligned cuboid, optionally rolled about Z (for ramps).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LevelBox {
    pub center: [f32; 3],
    pub half: [f32; 3],
    /// Radians about the Z axis; positive tilts +X up.
    pub rot_z: f32,
}

impl LevelBox {
    pub fn new(center: [f32; 3], half: [f32; 3]) -> Self {
        Self { center, half, rot_z: 0.0 }
    }

    pub fn rolled(mut self, rot_z: f32) -> Self {
        self.rot_z = rot_z;
        self
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct LevelDesc {
    pub boxes: Vec<LevelBox>,
    /// Round-robin spawn points (capsule centre).
    pub spawns: Vec<[f32; 3]>,
}

/// Where the shipped arena lives relative to the repo / install root.
pub const DEFAULT_LEVEL: &str = "levels/arena.lvl";

/// The default level file when one exists: `levels/arena.lvl` from the working directory, then upward from the
/// executable (so `target/release/shinobi-server` finds the repo's `levels/` however it was launched). Server and
/// game must use this same rule, or the client predicts against a different world than the server runs.
/// `None` means "use `LevelDesc::standard()`".
pub fn default_level_path() -> Option<std::path::PathBuf> {
    find_level(std::path::Path::new(DEFAULT_LEVEL), std::env::current_exe().ok().as_deref())
}

/// `rel` against the working directory, then against the first five ancestors of `exe`. Testable core of
/// `default_level_path`.
pub fn find_level(rel: &std::path::Path, exe: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    if rel.is_file() {
        return Some(rel.to_path_buf());
    }
    exe?.ancestors().skip(1).take(5).map(|d| d.join(rel)).find(|p| p.is_file())
}

/// The one place a static box becomes a collider.
pub fn insert_static_box(w: &mut PhysicsWorld, b: &LevelBox) {
    w.colliders.insert(
        ColliderBuilder::cuboid(b.half[0], b.half[1], b.half[2])
            .position(Isometry::new(vector![b.center[0], b.center[1], b.center[2]], vector![0.0, 0.0, b.rot_z]))
            .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
            .build(),
    );
}

/// A pickup point from a level file (server-only: the client never needs it to predict movement).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PickupPoint {
    pub pos: [f32; 3],
    pub respawn_secs: f32,
}

/// Everything a level file says. `LevelDesc` is the part both sides need.
#[derive(Clone, PartialEq, Debug)]
pub struct LevelFile {
    pub level: LevelDesc,
    pub pickups: Vec<PickupPoint>,
}

pub const DEFAULT_PICKUP_RESPAWN_SECS: f32 = 20.0;

/// Parse a level file: one directive per line, `#` starts a comment.
/// ```text
/// box    cx cy cz  hx hy hz [rot_z_radians]   # static cuboid (centre, half extents), optional roll for ramps
/// spawn  x y z [team]                          # capsule-centre spawn point; team 0 Cyan / 1 Orange, omitted = either
/// pickup x y z [respawn_secs]                  # power-up cube, default respawn 20 s
/// ```
/// Errors name the line. A level needs at least one `box` and one `spawn`.
pub fn parse_level_file(text: &str) -> Result<LevelFile, String> {
    let (mut boxes, mut spawns, mut pickups) = (Vec::new(), Vec::new(), Vec::new());
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let err = |m: &str| format!("line {}: {m}: `{line}`", n + 1);
        let mut it = line.split_whitespace();
        let kind = it.next().unwrap();
        let nums: Vec<f32> =
            it.map(|t| t.parse::<f32>().map_err(|_| err("not a number"))).collect::<Result<_, _>>()?;
        if nums.iter().any(|v| !v.is_finite()) {
            return Err(err("non-finite number"));
        }
        match (kind, nums.len()) {
            ("box", 6 | 7) => {
                if nums[3..6].iter().any(|h| *h <= 0.0) {
                    return Err(err("half extents must be > 0"));
                }
                let b = LevelBox::new([nums[0], nums[1], nums[2]], [nums[3], nums[4], nums[5]]);
                boxes.push(if nums.len() == 7 { b.rolled(nums[6]) } else { b });
            }
            ("spawn", 3 | 4) => {
                if nums.len() == 4 && !(nums[3] == 0.0 || nums[3] == 1.0) {
                    return Err(err("spawn team must be 0 (Cyan) or 1 (Orange)"));
                }
                spawns.push([nums[0], nums[1], nums[2]]);
            }
            ("pickup", 3 | 4) => pickups.push(PickupPoint {
                pos: [nums[0], nums[1], nums[2]],
                respawn_secs: nums.get(3).copied().unwrap_or(DEFAULT_PICKUP_RESPAWN_SECS),
            }),
            ("box" | "spawn" | "pickup", _) => return Err(err("wrong number of values")),
            _ => return Err(err("unknown directive (box | spawn | pickup)")),
        }
    }
    if boxes.is_empty() {
        return Err("level has no `box` (nothing to stand on)".into());
    }
    if spawns.is_empty() {
        return Err("level has no `spawn`".into());
    }
    Ok(LevelFile { level: LevelDesc { boxes, spawns }, pickups })
}

/// Team of each `spawn` line of a level file, in order (`NO_TEAM` when the line has no team). Parallel to
/// `LevelDesc::spawns`. Kept apart from `LevelDesc` so the client's copy of the level is unchanged; call after
/// `parse_level_file` accepted the text.
pub fn parse_spawn_teams(text: &str) -> Vec<u8> {
    text.lines()
        .filter_map(|raw| {
            let mut it = raw.split('#').next()?.split_whitespace();
            (it.next()? == "spawn").then(|| it.nth(3).and_then(|t| t.parse::<u8>().ok()).unwrap_or(crate::tick::NO_TEAM))
        })
        .collect()
}

impl LevelFile {
    /// Inverse of `parse_level_file`: `parse(to_text(x)) == x` exactly (f32 `Display` round-trips).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for b in &self.level.boxes {
            let [cx, cy, cz] = b.center;
            let [hx, hy, hz] = b.half;
            out += &format!("box {cx} {cy} {cz} {hx} {hy} {hz}");
            if b.rot_z != 0.0 {
                out += &format!(" {}", b.rot_z);
            }
            out.push('\n');
        }
        for [x, y, z] in &self.level.spawns {
            out += &format!("spawn {x} {y} {z}\n");
        }
        for p in &self.pickups {
            out += &format!("pickup {} {} {} {}\n", p.pos[0], p.pos[1], p.pos[2], p.respawn_secs);
        }
        out
    }
}

impl LevelDesc {
    /// Geometry and spawns from level-file text (the client's entry point; `pickup` lines are accepted and dropped).
    pub fn from_text(text: &str) -> Result<Self, String> {
        parse_level_file(text).map(|f| f.level)
    }

    /// Level-file text for just the geometry and spawns.
    pub fn to_text(&self) -> String {
        LevelFile { level: self.clone(), pickups: Vec::new() }.to_text()
    }

    /// What the server runs today: one big floor.
    pub fn standard() -> Self {
        Self {
            boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0])],
            spawns: vec![[0.0, 0.81, 0.0], [6.0, 0.81, 0.0], [-6.0, 0.81, 0.0], [0.0, 0.81, 6.0]],
        }
    }

    pub fn spawn(&self, i: usize) -> [f32; 3] {
        self.spawns[i % self.spawns.len()]
    }

    pub fn insert_into(&self, w: &mut PhysicsWorld) {
        for b in &self.boxes {
            insert_static_box(w, b);
        }
    }

    pub fn apply_to_server(&self, s: &mut Server) {
        self.insert_into(&mut s.world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Cfg;

    #[test]
    fn server_and_client_worlds_get_identical_colliders() {
        let level = LevelDesc {
            boxes: vec![
                LevelBox::new([0.0, -0.5, 0.0], [50.0, 0.5, 50.0]),
                LevelBox::new([5.0, 1.0, 0.0], [1.0, 1.0, 4.0]).rolled(0.3),
            ],
            spawns: vec![[0.0, 0.81, 0.0]],
        };
        let mut server = Server::new(Cfg::default());
        level.apply_to_server(&mut server);
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        level.insert_into(&mut w);
        let a: Vec<_> = server.world.colliders.iter().map(|(_, c)| (*c.position(), c.shape().as_cuboid().unwrap().half_extents)).collect();
        let b: Vec<_> = w.colliders.iter().map(|(_, c)| (*c.position(), c.shape().as_cuboid().unwrap().half_extents)).collect();
        assert_eq!(a, b);
        assert!((a[1].0.rotation.angle() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn parse_good_file() {
        let f = parse_level_file(
            "# arena\nbox 0 -0.5 0  50 0.5 50\nbox 5 1 0 1 1 4 0.3  # ramp\nspawn 0 0.81 0\nspawn 4 0.81 4\npickup 8 1 0\npickup -8 1 0 5\n",
        )
        .unwrap();
        assert_eq!((f.level.boxes.len(), f.level.spawns.len(), f.pickups.len()), (2, 2, 2));
        assert_eq!(f.level.boxes[1].rot_z, 0.3);
        assert_eq!((f.pickups[0].respawn_secs, f.pickups[1].respawn_secs), (20.0, 5.0));
    }

    #[test]
    fn parse_errors_name_the_line() {
        for (text, needle) in [
            ("box 0 0 0 1 1\nspawn 0 0 0", "line 1: wrong number"),
            ("box 0 0 0 1 1 1\nspawn 0 x 0", "line 2: not a number"),
            ("box 0 0 0 1 0 1\nspawn 0 0 0", "half extents"),
            ("box 0 0 0 1 1 1\nwarp 1 2 3", "line 2: unknown directive"),
            ("box 0 0 0 1 1 1", "no `spawn`"),
            ("spawn 0 0 0", "no `box`"),
            ("box 0 0 0 1 1 1\nspawn nan 0 0", "non-finite"),
        ] {
            let e = parse_level_file(text).unwrap_err();
            assert!(e.contains(needle), "{text:?} -> {e}");
        }
    }

    #[test]
    fn text_roundtrip_is_exact_and_geometry_only_view_drops_pickups() {
        let f = LevelFile {
            level: LevelDesc {
                boxes: vec![
                    LevelBox::new([0.1, -0.5, 1.0 / 3.0], [50.0, 0.5, 50.0]),
                    LevelBox::new([5.0, 1.0, 0.0], [1.0, 1.0, 4.0]).rolled(-0.35),
                ],
                spawns: vec![[0.0, 0.81, 0.0], [4.25, 0.81, -4.0]],
            },
            pickups: vec![PickupPoint { pos: [8.0, 1.0, 0.0], respawn_secs: 5.5 }],
        };
        assert_eq!(parse_level_file(&f.to_text()).unwrap(), f);
        assert_eq!(LevelDesc::from_text(&f.to_text()).unwrap(), f.level);
        assert_eq!(LevelDesc::from_text(&f.level.to_text()).unwrap(), f.level);
        assert_eq!(LevelDesc::from_text(&LevelDesc::standard().to_text()).unwrap(), LevelDesc::standard());
    }

    #[test]
    fn find_level_looks_upward_from_the_executable_and_stops_after_five_levels() {
        let root = std::env::temp_dir().join(format!("shinobi-lvl-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("levels")).unwrap();
        std::fs::write(root.join(DEFAULT_LEVEL), "box 0 -0.5 0 10 0.5 10\nspawn 0 0.81 0\n").unwrap();
        let rel = std::path::Path::new(DEFAULT_LEVEL);
        // target/release/shinobi-server: root is 3 ancestors up
        let exe = root.join("target").join("release").join("shinobi-server");
        assert_eq!(find_level(rel, Some(&exe)), Some(root.join(DEFAULT_LEVEL)));
        // exe nested too deep: not found
        let deep = root.join("a").join("b").join("c").join("d").join("e").join("f").join("exe");
        assert_eq!(find_level(rel, Some(&deep)), None);
        assert_eq!(find_level(rel, None), None, "no exe, no cwd copy: built-in");
        assert_eq!(find_level(std::path::Path::new("levels/does-not-exist.lvl"), Some(&exe)), None);
        // and what it finds parses
        assert!(LevelDesc::from_text(&std::fs::read_to_string(root.join(DEFAULT_LEVEL)).unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }
}
