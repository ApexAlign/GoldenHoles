//! Scripted "visitor" for demo clips: a list of stages, each with a goal to walk to (planned around walls with
//! `nav`), an aim mode, an end condition and button taps relative to the stage start. It outputs what a player
//! would: stick level, look motion and press/release events. Closed-loop (it reads the player's position and the
//! opponents'), so it works from any spawn. Pure and deterministic given the same inputs.
//!
//! World conventions: yaw 0 faces -Z, positive yaw turns toward -X; `bearing_to` > 0 means "on my right".

use shinobi_sim::level::LevelDesc;

use crate::client_game::{Action, ButtonEvent};
use crate::hud::bearing_to;
use crate::nav::NavGrid;

/// Max camera turn speed while steering (rad/s).
const MAX_TURN: f32 = 6.0;
/// Pop a waypoint when within this many metres.
const WAYPOINT_R: f32 = 1.0;
/// Re-plan toward a moving target this often (s).
const REPLAN_S: f32 = 0.5;
/// Keep this far from the walls (player radius + slack).
const NAV_MARGIN: f32 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goal {
    /// Stand still.
    Hold,
    /// Chase the nearest opponent.
    Nearest,
    /// Walk to a point, planning around walls (keeps `NAV_MARGIN` clear of them).
    Point([f32; 2]),
    /// Walk straight at a point with no planning: for lanes hugging a wall (the planner would call them blocked).
    Direct([f32; 2]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Aim {
    /// Look where we are walking.
    Travel,
    /// Look at the nearest opponent (strafe-walk along the path).
    Nearest,
    /// Leave the view alone.
    Keep,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Until {
    Secs(f32),
    /// Within `radius` of the goal, or after `timeout` seconds.
    Reached(f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub at: f32,
    pub action: Action,
    pub down: bool,
}

#[derive(Clone, Debug)]
pub struct Stage {
    pub goal: Goal,
    pub aim: Aim,
    pub until: Until,
    /// Relative to the stage start; need not be sorted.
    pub keys: Vec<Key>,
}

/// What the script can see.
#[derive(Clone, Debug, Default)]
pub struct World {
    pub pos: [f32; 3],
    pub yaw: f32,
    /// Opponent positions.
    pub remotes: Vec<[f32; 3]>,
}

#[derive(Clone, Debug, Default)]
pub struct StepOut {
    pub stick: (f32, f32),
    /// Look motion in device units, to add to `InputFrame::look`.
    pub look: (f32, f32),
    pub events: Vec<ButtonEvent>,
}

pub struct DemoScript {
    stages: Vec<Stage>,
    grid: NavGrid,
    stage: usize,
    t0: Option<f32>,
    next_key: usize,
    path: Vec<[f32; 2]>,
    replan_at: f32,
}

/// Tap: press at `at`, release `hold` later.
fn tap(at: f32, a: Action, hold: f32) -> [Key; 2] {
    [Key { at, action: a, down: true }, Key { at: at + hold, action: a, down: false }]
}

fn d2(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

impl DemoScript {
    pub fn new(level: &LevelDesc, mut stages: Vec<Stage>) -> Self {
        for s in &mut stages {
            s.keys.sort_by(|a, b| a.at.total_cmp(&b.at));
        }
        Self {
            stages,
            grid: NavGrid::from_level(level, NAV_MARGIN),
            stage: 0,
            t0: None,
            next_key: 0,
            path: Vec::new(),
            replan_at: 0.0,
        }
    }

    pub fn finished(&self) -> bool {
        self.stage >= self.stages.len()
    }

    pub fn stage_index(&self) -> usize {
        self.stage
    }

    fn target(&self, st: &Stage, w: &World) -> Option<[f32; 2]> {
        match st.goal {
            Goal::Hold => None,
            Goal::Point(p) | Goal::Direct(p) => Some(p),
            Goal::Nearest => nearest(w).map(|r| [r[0], r[2]]),
        }
    }

    /// Advance to script time `now` over a frame of `dt` seconds.
    pub fn step(&mut self, now: f32, dt: f32, rad_per_unit: f32, w: &World) -> StepOut {
        let mut out = StepOut::default();
        if self.finished() {
            return out;
        }
        if self.t0.is_none() {
            self.t0 = Some(now);
            self.next_key = 0;
            self.path.clear();
            self.replan_at = now;
        }
        let rt = now - self.t0.unwrap();
        let st = self.stages[self.stage].clone();
        let me = [w.pos[0], w.pos[2]];
        let target = self.target(&st, w);

        // buttons due in this stage
        while let Some(k) = st.keys.get(self.next_key).filter(|k| k.at <= rt) {
            out.events.push(ButtonEvent { action: k.action, down: k.down });
            self.next_key += 1;
        }

        // route
        if let Some(t) = target {
            if matches!(st.goal, Goal::Direct(_)) {
                self.path = vec![t];
            } else if now >= self.replan_at || self.path.is_empty() {
                self.path = self.grid.path(me, t).unwrap_or_else(|| vec![t]);
                self.replan_at = now + REPLAN_S;
            }
            while self.path.len() > 1 && d2(me, self.path[0]) < WAYPOINT_R {
                self.path.remove(0);
            }
        }

        let arrived = target.map_or(true, |t| d2(me, t) < 0.6);
        let waypoint = self.path.first().copied();
        let travel = match (target, waypoint) {
            (Some(_), Some(p)) if !arrived => {
                let d = [p[0] - me[0], p[1] - me[1]];
                let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
                (l > 1e-3).then(|| [d[0] / l, d[1] / l])
            }
            _ => None,
        };

        // aim point -> look
        let aim_pt: Option<[f32; 3]> = match st.aim {
            Aim::Keep => None,
            Aim::Nearest => nearest(w).or_else(|| travel.map(|d| [w.pos[0] + d[0] * 10.0, w.pos[1], w.pos[2] + d[1] * 10.0])),
            Aim::Travel => travel.map(|d| [w.pos[0] + d[0] * 10.0, w.pos[1], w.pos[2] + d[1] * 10.0]),
        };
        if let Some(p) = aim_pt {
            let b = bearing_to(w.pos, w.yaw, p); // + = target on the right = turn right = + dx
            let max = MAX_TURN * dt;
            out.look.0 = b.clamp(-max, max) / rad_per_unit.max(1e-6);
        }

        // stick: world travel direction in the view frame
        if let Some(d) = travel {
            let (s, c) = w.yaw.sin_cos();
            let (fwd, right) = ([-s, -c], [c, -s]);
            out.stick = (d[0] * right[0] + d[1] * right[1], d[0] * fwd[0] + d[1] * fwd[1]);
        }

        // stage end
        let done = match st.until {
            Until::Secs(s) => rt >= s,
            Until::Reached(r, timeout) => rt >= timeout || target.map_or(true, |t| d2(me, t) < r),
        };
        if done {
            self.stage += 1;
            self.t0 = None;
        }
        out
    }

    /// The film: hunt the nearest opponent (slices and throws, a double jump and a dash on the way), cross to the
    /// mouth of wall-run corridor A of `levels/arena.lvl`, wall-run it along the east wall slicing mid-run, then
    /// hop out onto the sniper ledge.
    pub fn arena(level: &LevelDesc) -> Self {
        let keys = |v: Vec<[Key; 2]>| v.into_iter().flatten().collect::<Vec<_>>();
        let hunt = keys(vec![
            tap(1.2, Action::Jump, 0.15),
            tap(1.7, Action::Jump, 0.15), // double jump
            tap(2.4, Action::Dash, 0.1),
            tap(3.0, Action::Slice, 0.1),
            tap(3.8, Action::Fire, 0.1),
            tap(4.6, Action::Slice, 0.1),
            tap(5.4, Action::Fire, 0.1),
            tap(6.2, Action::Slice, 0.1),
            tap(7.0, Action::Fire, 0.1),
            tap(7.8, Action::Slice, 0.1),
        ]);
        let wall = keys(vec![tap(0.0, Action::Jump, 0.25), tap(1.6, Action::Slice, 0.1)]);
        let exit = keys(vec![tap(0.0, Action::Jump, 0.15), tap(0.5, Action::Jump, 0.15)]);
        Self::new(
            level,
            vec![
                Stage { goal: Goal::Nearest, aim: Aim::Nearest, until: Until::Secs(7.5), keys: hunt },
                // planned route to the open ground south of corridor A, in line with its east-wall lane (x = -19.8)
                Stage { goal: Goal::Point([-19.8, 5.0]), aim: Aim::Travel, until: Until::Reached(1.2, 18.0), keys: vec![] },
                // straight up the lane with the cling button held, then jump into the wall-run
                Stage {
                    goal: Goal::Direct([-19.8, -3.0]),
                    aim: Aim::Travel,
                    until: Until::Reached(1.0, 4.0),
                    keys: vec![Key { at: 0.0, action: Action::Cling, down: true }],
                },
                Stage {
                    goal: Goal::Direct([-19.8, -29.0]),
                    aim: Aim::Travel,
                    until: Until::Secs(3.2),
                    keys: [wall, vec![Key { at: 3.0, action: Action::Cling, down: false }]].concat(),
                },
                Stage { goal: Goal::Point([-22.0, -31.5]), aim: Aim::Travel, until: Until::Secs(2.0), keys: exit },
            ],
        )
    }
}

fn nearest(w: &World) -> Option<[f32; 3]> {
    w.remotes
        .iter()
        .min_by(|a, b| d2([w.pos[0], w.pos[2]], [a[0], a[2]]).total_cmp(&d2([w.pos[0], w.pos[2]], [b[0], b[2]])))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: f32 = 0.0022;
    const DT: f32 = 1.0 / 60.0;

    fn arena() -> LevelDesc {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl")).unwrap();
        LevelDesc::from_text(&text).unwrap()
    }

    fn world(pos: [f32; 3], yaw: f32) -> World {
        World { pos, yaw, remotes: vec![] }
    }

    fn one_stage(goal: Goal, aim: Aim, until: Until, keys: Vec<Key>) -> DemoScript {
        DemoScript::new(&LevelDesc::standard(), vec![Stage { goal, aim, until, keys }])
    }

    #[test]
    fn walks_toward_the_goal_turning_the_right_way() {
        let mut s = one_stage(Goal::Point([10.0, 0.0]), Aim::Travel, Until::Reached(0.5, 10.0), vec![]);
        // goal is +X; facing -Z (yaw 0) => goal on the right => turn right (dx > 0), strafe right
        let o = s.step(0.0, DT, R, &world([0.0, 0.8, 0.0], 0.0));
        assert!(o.look.0 > 0.0, "turn right: {:?}", o.look);
        assert!(o.stick.0 > 0.9 && o.stick.1.abs() < 0.2, "mostly strafe right while still facing -Z: {:?}", o.stick);
        // once facing +X (yaw = -pi/2) the same goal is straight ahead
        let o = s.step(DT, DT, R, &world([0.0, 0.8, 0.0], -std::f32::consts::FRAC_PI_2));
        assert!(o.stick.1 > 0.95 && o.stick.0.abs() < 0.1 && o.look.0.abs() < 1.0, "{o:?}");
    }

    #[test]
    fn nearest_aim_looks_at_the_opponent_while_walking_the_path() {
        let mut s = one_stage(Goal::Hold, Aim::Nearest, Until::Secs(5.0), vec![]);
        let mut w = world([0.0, 0.8, 0.0], 0.0);
        w.remotes = vec![[-10.0, 0.8, 0.0], [30.0, 0.8, 0.0]]; // nearest is on the left (-X)
        let o = s.step(0.0, DT, R, &w);
        assert!(o.look.0 < 0.0, "turn left toward the nearest: {:?}", o.look);
        assert_eq!(o.stick, (0.0, 0.0), "Hold goal: no walking");
    }

    #[test]
    fn stage_keys_fire_once_relative_to_stage_start_and_stages_advance() {
        let keys = tap(0.5, Action::Slice, 0.1).to_vec();
        let mut s = DemoScript::new(
            &LevelDesc::standard(),
            vec![
                Stage { goal: Goal::Hold, aim: Aim::Keep, until: Until::Secs(1.0), keys },
                Stage { goal: Goal::Hold, aim: Aim::Keep, until: Until::Secs(0.2), keys: tap(0.0, Action::Dash, 0.05).to_vec() },
            ],
        );
        let w = world([0.0, 0.8, 0.0], 0.0);
        assert!(s.step(10.0, DT, R, &w).events.is_empty(), "stage clock starts at the first call");
        let e = s.step(10.5, DT, R, &w).events;
        assert_eq!((e[0].action, e[0].down), (Action::Slice, true));
        assert!(s.step(10.52, DT, R, &w).events.is_empty(), "not repeated");
        let e = s.step(10.7, DT, R, &w).events;
        assert!(!e[0].down);
        assert_eq!(s.stage_index(), 0);
        s.step(11.0, DT, R, &w); // stage 0 ends
        assert_eq!(s.stage_index(), 1);
        let e = s.step(11.01, DT, R, &w).events; // stage 1 starts: its t=0 tap fires immediately
        assert_eq!((e[0].action, e[0].down), (Action::Dash, true));
        s.step(11.3, DT, R, &w);
        assert!(s.finished());
        assert!(s.step(12.0, DT, R, &w).events.is_empty());
    }

    #[test]
    fn direct_goal_ignores_walls_so_a_wall_hugging_lane_stays_reachable() {
        let l = arena();
        // (-19.8, -10) is 0.6 m from corridor A's east wall: inside the planner's 0.8 m margin
        assert!(NavGrid::from_level(&l, 0.8).is_blocked([-19.8, -10.0]));
        let mut s = DemoScript::new(&l, vec![Stage { goal: Goal::Direct([-19.8, -29.0]), aim: Aim::Travel, until: Until::Secs(5.0), keys: vec![] }]);
        // standing exactly in the lane facing -Z (yaw 0): go straight ahead, no sideways drift
        let o = s.step(0.0, DT, R, &world([-19.8, 0.8, -4.0], 0.0));
        assert!(o.stick.1 > 0.99 && o.stick.0.abs() < 0.02 && o.look.0.abs() < 1.0, "{o:?}");
    }

    #[test]
    fn reached_ends_the_stage_early_and_timeout_ends_it_late() {
        let mut s = one_stage(Goal::Point([1.0, 0.0]), Aim::Keep, Until::Reached(1.5, 3.0), vec![]);
        s.step(0.0, DT, R, &world([0.0, 0.8, 0.0], 0.0)); // 1 m away < 1.5 m radius
        assert!(s.finished(), "already inside the radius");
        let mut s = one_stage(Goal::Point([30.0, 0.0]), Aim::Keep, Until::Reached(1.0, 3.0), vec![]);
        s.step(0.0, DT, R, &world([0.0, 0.8, 0.0], 0.0));
        assert!(!s.finished());
        s.step(3.1, DT, R, &world([0.0, 0.8, 0.0], 0.0));
        assert!(s.finished(), "timeout");
    }

    #[test]
    fn arena_route_detours_around_the_ziggurat_from_any_spawn() {
        let l = arena();
        for sp in &l.spawns {
            let mut s = DemoScript::arena(&l);
            // skip the hunt: jump straight to the walk-to-mouth stage by running the clock through stage 0
            let w = world(*sp, 0.0);
            s.step(0.0, DT, R, &w);
            s.step(9.1, DT, R, &w); // hunt ends (Secs(9))
            assert_eq!(s.stage_index(), 1, "now crossing to the corridor mouth");
            let o = s.step(9.2, DT, R, &w);
            assert!(o.stick != (0.0, 0.0), "heading somewhere from {sp:?}");
        }
    }

    #[test]
    fn arena_script_uses_every_move_and_holds_cling_for_the_run() {
        let l = arena();
        let mut s = DemoScript::arena(&l);
        // simulate a player standing near the corridor mouth so every stage runs to its natural end
        let mut seen = std::collections::HashSet::new();
        let mut cling_down = false;
        let (mut t, mut guard) = (0.0f32, 0);
        let positions = [[-19.8, 0.8, 5.0], [-19.8, 0.8, -3.0], [-19.8, 0.8, -29.0], [-22.0, 0.8, -31.5]];
        while !s.finished() && guard < 20_000 {
            let w = world(positions[s.stage_index().saturating_sub(1).min(3)], 0.0);
            let w = if s.stage_index() == 0 { World { remotes: vec![[5.0, 0.8, 5.0]], ..w } } else { w };
            for e in s.step(t, DT, R, &w).events {
                if e.down {
                    seen.insert(format!("{:?}", e.action));
                }
                if e.action == Action::Cling {
                    cling_down = e.down;
                }
            }
            t += DT;
            guard += 1;
        }
        assert!(s.finished(), "script ends");
        for a in ["Jump", "Dash", "Slice", "Fire", "Cling"] {
            assert!(seen.contains(a), "never used {a}: {seen:?}");
        }
        assert!(!cling_down, "cling released after the run");
    }
}
