//! Pickup-only ninja power weapons (docs/powerups.md): the YUMI (digital bow) and the EXPLOSIVE TAG (fuda).
//!
//! Nobody starts with one. A weapon lives in the player's hands (`Player::power`), is taken from a pedestal or from a dead
//! player's drop (`pickups.rs`), and replaces the right mouse button while held. This file is the pure part: the carried
//! state machine (`PowerState`: draw timer, ammo, carry timer), the tunables (`combat.yumi.*`, `combat.tag.*`) and the
//! geometry helpers. It is a function of ticks only and the client's predictor runs the very same code, so the draw ring,
//! the slowed walk and the moment of release agree on both sides (`tests/power_parity.rs`). Arrows in flight, stuck tags and
//! the blast live on the server (`power_pass.rs`).

/// Which weapon is in the hands. Wire value = discriminant (2 bits).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PowerKind {
    #[default]
    None = 0,
    Yumi = 1,
    Tag = 2,
}

impl PowerKind {
    pub fn from_u8(v: u8) -> Self {
        match v & 3 {
            1 => Self::Yumi,
            2 => Self::Tag,
            _ => Self::None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Yumi => "YUMI",
            Self::Tag => "FUDA TAG",
        }
    }
}

/// `combat.yumi.*` (Balance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YumiCfg {
    /// Seconds the bow can be carried, arrows per pickup.
    pub carry_s: f32,
    pub arrows: u32,
    /// Full draw time; releasing before it cancels the draw (no arrow spent). A release inside `tap_s` is a tap: the
    /// button press counts as an ordinary star throw instead.
    pub draw_s: f32,
    pub tap_s: f32,
    /// Walk speed multiplier while the bow is held out, and the look-speed multiplier the client applies (slow turn assist).
    pub move_mult: f32,
    pub turn_mult: f32,
    /// Arrow speed (m/s), gravity on it (m/s^2: the slight drop), range (m) and the radius of its hit reach (m).
    pub speed: f32,
    pub gravity: f32,
    pub range: f32,
    pub radius: f32,
    /// Body damage; a hit above `combat.head_min_height` multiplies it by `head_mult`.
    pub damage: f32,
    pub head_mult: f32,
    /// Fraction of the victim's guard a blocked arrow spends (a spent guard breaks); a blocked arrow's damage is cut to the
    /// guard's `front_mult` like any hit.
    pub block_drain: f32,
    /// A katana swing whose blade passes this close (beyond its own half width) to an arrow cuts it down.
    pub cut_pad: f32,
    /// Seconds between loosing an arrow and the next draw.
    pub cooldown_s: f32,
    /// How long the bow lies on the floor after its owner died.
    pub drop_life_s: f32,
}

impl Default for YumiCfg {
    fn default() -> Self {
        Self {
            carry_s: 30.0,
            arrows: 3,
            draw_s: 0.6,
            tap_s: 0.15,
            move_mult: 0.75,
            turn_mult: 0.5,
            speed: 90.0,
            gravity: 5.0,
            range: 140.0,
            radius: 0.35,
            damage: 80.0,
            head_mult: 1.5,
            block_drain: 0.7,
            cut_pad: 0.35,
            cooldown_s: 0.5,
            drop_life_s: 25.0,
        }
    }
}

/// `combat.tag.*` (Balance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagCfg {
    /// Seconds the tags can be carried, tags per pickup, tags live per player.
    pub carry_s: f32,
    pub count: u32,
    pub max_live: u32,
    /// Throw speed (m/s), gravity on a flying tag, and how long it may fly before it fizzles (s).
    pub throw_speed: f32,
    pub gravity: f32,
    pub flight_s: f32,
    /// Seconds after sticking until the tag is armed (a second right click can set it off) and until it goes off by itself.
    pub arm_s: f32,
    pub fuse_s: f32,
    /// Blast radius (m), centre damage, share of it left at the edge, knockback speed (m/s, ring-out strength), and the share
    /// of damage the thrower takes from their own blast (teammates take none).
    pub radius: f32,
    pub damage: f32,
    pub edge_frac: f32,
    pub knock: f32,
    pub self_mult: f32,
    /// A katana swing whose blade passes this close (beyond its own half width) to the tag cuts it off.
    pub cut_pad: f32,
    pub cooldown_s: f32,
    /// Reach of a tag's sticking volume against a player capsule (m), on top of the capsule radius.
    pub stick_radius: f32,
    pub drop_life_s: f32,
}

impl Default for TagCfg {
    fn default() -> Self {
        Self {
            carry_s: 60.0,
            count: 2,
            max_live: 2,
            throw_speed: 24.0,
            gravity: 9.0,
            flight_s: 3.0,
            arm_s: 0.5,
            fuse_s: 2.0,
            radius: 4.0,
            damage: 50.0,
            edge_frac: 0.4,
            knock: 14.0,
            self_mult: 0.3,
            cut_pad: 0.45,
            cooldown_s: 0.4,
            stick_radius: 0.3,
            drop_life_s: 25.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PowerCfg {
    pub yumi: YumiCfg,
    pub tag: TagCfg,
}

impl PowerCfg {
    /// Ammo handed out by a fresh pickup of `kind` and the seconds it can be carried.
    pub fn fresh(&self, kind: PowerKind) -> (u8, f32) {
        match kind {
            PowerKind::Yumi => (self.yumi.arrows.min(9) as u8, self.yumi.carry_s),
            PowerKind::Tag => (self.tag.count.min(9) as u8, self.tag.carry_s),
            PowerKind::None => (0, 0.0),
        }
    }

    pub fn drop_life_s(&self, kind: PowerKind) -> f32 {
        match kind {
            PowerKind::Yumi => self.yumi.drop_life_s,
            _ => self.tag.drop_life_s,
        }
    }
}

/// What the state machine sees each tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct PowerInput {
    /// Right button held (yumi draw).
    pub draw: bool,
    /// Right button pressed this tick (an edge: tag throw / detonate).
    pub fire: bool,
    /// Live tags of this player, and how many of them are armed (a press with no tags left in hand sets the armed ones off).
    pub live_tags: u32,
    pub armed_tags: u32,
    /// Alive, in a live match, no shield up, not staggered.
    pub can_act: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    /// The bow came up (a draw began).
    DrawStart,
    /// The draw ended before full: nothing fired. `tap`: shorter than `tap_s`, so the press is a star throw after all.
    DrawCancel { tap: bool },
    /// A full draw was released: an arrow leaves the bow.
    Loose,
    /// A tag leaves the hand.
    TagThrow,
    /// A press with the hands empty sets off the armed tags.
    TagDetonate,
    /// The carry timer ran out; the weapon is gone.
    Expired,
    /// The last arrow / tag is spent; the weapon is gone.
    Spent,
}

/// The weapon in a player's hands. Server and client predictor step it with the same inputs.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PowerState {
    pub kind: PowerKind,
    /// Arrows / tags left in hand.
    pub ammo: u8,
    /// Carry seconds left.
    pub left: f32,
    /// Yumi: drawing now, seconds drawn.
    pub drawing: bool,
    pub draw: f32,
    pub cooldown: f32,
    /// The press of this tick was used up by the weapon (no star may be thrown with it). Reset every `update`.
    pub used_fire: bool,
}

impl PowerState {
    pub fn held(&self) -> bool {
        self.kind != PowerKind::None
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Take a weapon into empty hands. `ammo`/`secs` of 0 mean a fresh load.
    pub fn grant(&mut self, kind: PowerKind, ammo: u8, secs: f32, cfg: &PowerCfg) -> bool {
        if self.held() || kind == PowerKind::None {
            return false;
        }
        let (a, s) = cfg.fresh(kind);
        *self = Self { kind, ammo: if ammo == 0 { a } else { ammo }, left: if secs <= 0.0 { s } else { secs }, ..Self::default() };
        true
    }

    /// Draw progress 0..=1 (the ring on the HUD); 0 when not drawing.
    pub fn draw_frac(&self, cfg: &PowerCfg) -> f32 {
        if self.drawing && cfg.yumi.draw_s > 0.0 {
            (self.draw / cfg.yumi.draw_s).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Fully drawn: releasing now looses the arrow.
    pub fn full(&self, cfg: &PowerCfg) -> bool {
        self.drawing && self.draw + 1e-4 >= cfg.yumi.draw_s
    }

    /// No star may leave the hand this tick: the bow is up, an arrow just left, or the press went to a tag.
    pub fn blocks_star(&self) -> bool {
        self.drawing || self.used_fire
    }

    /// Advance one tick.
    pub fn update(&mut self, dt: f32, inp: &PowerInput, cfg: &PowerCfg, out: &mut Vec<PowerEvent>) {
        self.used_fire = false;
        if !self.held() {
            return;
        }
        self.left -= dt;
        self.cooldown = (self.cooldown - dt).max(0.0);
        if self.left <= 0.0 {
            self.clear();
            out.push(PowerEvent::Expired);
            return;
        }
        match self.kind {
            PowerKind::Yumi => {
                let want = inp.draw && inp.can_act;
                if self.drawing {
                    if want {
                        self.draw = (self.draw + dt).min(cfg.yumi.draw_s);
                    } else {
                        let full = self.full(cfg);
                        let tap = self.draw < cfg.yumi.tap_s;
                        self.drawing = false;
                        self.draw = 0.0;
                        if full && inp.can_act {
                            self.used_fire = true;
                            self.ammo = self.ammo.saturating_sub(1);
                            self.cooldown = cfg.yumi.cooldown_s;
                            out.push(PowerEvent::Loose);
                            if self.ammo == 0 {
                                self.clear();
                                self.used_fire = true;
                                out.push(PowerEvent::Spent);
                            }
                        } else {
                            out.push(PowerEvent::DrawCancel { tap });
                        }
                    }
                } else if want && self.cooldown <= 0.0 && self.ammo > 0 {
                    self.drawing = true;
                    self.draw = 0.0;
                    out.push(PowerEvent::DrawStart);
                }
            }
            PowerKind::Tag => {
                if !inp.can_act {
                    return;
                }
                if inp.fire {
                    if self.ammo > 0 && self.cooldown <= 0.0 {
                        self.ammo -= 1;
                        self.cooldown = cfg.tag.cooldown_s;
                        self.used_fire = true;
                        out.push(PowerEvent::TagThrow);
                    } else if self.ammo > 0 {
                        self.used_fire = true; // mid-cooldown: swallow the press, do not turn it into a star
                    } else if inp.live_tags > 0 {
                        self.used_fire = true; // a tag is still counting down: the press is its trigger, never a star
                        if inp.armed_tags > 0 {
                            out.push(PowerEvent::TagDetonate);
                        }
                    }
                }
            }
            PowerKind::None => {}
        }
    }

    /// Tags: nothing left in hand and none live: the weapon is used up (the server decides, the client follows its report).
    pub fn spent_out(&mut self, live_tags: u32) -> bool {
        if self.kind == PowerKind::Tag && self.ammo == 0 && live_tags == 0 {
            self.clear();
            true
        } else {
            false
        }
    }
}

// ---- geometry -------------------------------------------------------------------------------------------

pub type V3 = [f32; 3];

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn scale(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn len(a: V3) -> f32 {
    libm::sqrtf(dot(a, a))
}
pub fn norm(a: V3) -> V3 {
    let l = len(a);
    if l < 1e-6 {
        [0.0, 0.0, -1.0]
    } else {
        scale(a, 1.0 / l)
    }
}

/// Closest approach of segments `p1..q1` and `p2..q2`: (distance, parameter along the first, 0..=1).
pub fn seg_seg(p1: V3, q1: V3, p2: V3, q2: V3) -> (f32, f32) {
    let (d1, d2, r) = (sub(q1, p1), sub(q2, p2), sub(p1, p2));
    let (a, e, f) = (dot(d1, d1), dot(d2, d2), dot(d2, r));
    let eps = 1e-9;
    let (s, t);
    if a <= eps && e <= eps {
        (s, t) = (0.0, 0.0);
    } else if a <= eps {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = dot(d1, r);
        if e <= eps {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = dot(d1, d2);
            let denom = a * e - b * b;
            let mut s_ = if denom > eps { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            let mut t_ = (b * s_ + f) / e;
            if t_ < 0.0 {
                t_ = 0.0;
                s_ = (-c / a).clamp(0.0, 1.0);
            } else if t_ > 1.0 {
                t_ = 1.0;
                s_ = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s_;
            t = t_;
        }
    }
    let c1 = add(p1, scale(d1, s));
    let c2 = add(p2, scale(d2, t));
    (len(sub(c1, c2)), s)
}

/// Distance from `p` to the segment `a..b`.
pub fn point_seg(a: V3, b: V3, p: V3) -> f32 {
    seg_seg(a, b, p, p).0
}

/// Blast damage at distance `d` from the centre: full at the centre, `edge_frac` at the radius, 0 outside.
pub fn blast_damage(cfg: &TagCfg, d: f32) -> f32 {
    if d > cfg.radius {
        return 0.0;
    }
    cfg.damage * (1.0 - (1.0 - cfg.edge_frac) * (d / cfg.radius.max(1e-3)).clamp(0.0, 1.0))
}

/// Launch velocity a blast at `centre` gives a victim at `pos`: `knock` m/s along the ground away from the centre (away from
/// `from` when the victim sits on the blast, e.g. a tag stuck to them), scaled down toward the edge, plus a hop.
pub fn blast_push(cfg: &TagCfg, centre: V3, from: V3, pos: V3) -> V3 {
    let d = len(sub(pos, centre));
    let k = cfg.knock * (1.0 - 0.5 * (d / cfg.radius.max(1e-3)).clamp(0.0, 1.0));
    let (mut dx, mut dz) = (pos[0] - centre[0], pos[2] - centre[2]);
    if libm::sqrtf(dx * dx + dz * dz) < 0.4 {
        dx = pos[0] - from[0];
        dz = pos[2] - from[2];
    }
    let l = libm::sqrtf(dx * dx + dz * dz);
    let (nx, nz) = if l < 1e-3 { (1.0, 0.0) } else { (dx / l, dz / l) };
    [nx * k, 4.0, nz * k]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn yumi() -> (PowerState, PowerCfg) {
        let cfg = PowerCfg::default();
        let mut s = PowerState::default();
        assert!(s.grant(PowerKind::Yumi, 0, 0.0, &cfg));
        (s, cfg)
    }

    fn run(s: &mut PowerState, cfg: &PowerCfg, n: u32, inp: PowerInput) -> Vec<PowerEvent> {
        let mut ev = Vec::new();
        for _ in 0..n {
            s.update(DT, &inp, cfg, &mut ev);
        }
        ev
    }

    const HOLD: PowerInput = PowerInput { draw: true, fire: false, live_tags: 0, armed_tags: 0, can_act: true };
    const IDLE: PowerInput = PowerInput { draw: false, fire: false, live_tags: 0, armed_tags: 0, can_act: true };

    #[test]
    fn a_full_draw_looses_exactly_one_arrow_and_a_short_one_cancels() {
        let (mut s, cfg) = yumi();
        let ev = run(&mut s, &cfg, 40, HOLD);
        assert_eq!(ev, vec![PowerEvent::DrawStart]);
        assert!(s.full(&cfg) && s.draw_frac(&cfg) > 0.999, "0.6 s at 60 Hz is 36 ticks");
        let ev = run(&mut s, &cfg, 1, IDLE);
        assert_eq!(ev, vec![PowerEvent::Loose]);
        assert_eq!(s.ammo, 2);
        assert!(s.used_fire && !s.drawing);
        // a half draw cancels and costs nothing; inside the tap time it is a tap
        let (mut s, cfg) = yumi();
        run(&mut s, &cfg, 20, HOLD);
        assert_eq!(run(&mut s, &cfg, 1, IDLE), vec![PowerEvent::DrawCancel { tap: false }]);
        assert_eq!(s.ammo, 3);
        let (mut s, cfg) = yumi();
        run(&mut s, &cfg, 4, HOLD);
        assert_eq!(run(&mut s, &cfg, 1, IDLE), vec![PowerEvent::DrawCancel { tap: true }]);
        assert!(!s.blocks_star(), "a tap leaves the star throw free");
    }

    #[test]
    fn the_bow_has_a_cooldown_runs_out_of_arrows_and_of_time() {
        let (mut s, cfg) = yumi();
        run(&mut s, &cfg, 37, HOLD);
        run(&mut s, &cfg, 1, IDLE);
        run(&mut s, &cfg, 5, HOLD);
        assert!(!s.drawing, "cooling down: no new draw yet");
        run(&mut s, &cfg, 30, IDLE);
        run(&mut s, &cfg, 1, HOLD);
        assert!(s.drawing);
        // last arrow spends the weapon
        let (mut s, cfg) = yumi();
        s.ammo = 1;
        run(&mut s, &cfg, 37, HOLD);
        let ev = run(&mut s, &cfg, 1, IDLE);
        assert_eq!(ev, vec![PowerEvent::Loose, PowerEvent::Spent]);
        assert!(!s.held());
        // carry time
        let (mut s, cfg) = yumi();
        let ev = run(&mut s, &cfg, 60 * 31, IDLE);
        assert_eq!(ev, vec![PowerEvent::Expired]);
        assert!(!s.held());
    }

    #[test]
    fn a_shield_or_stagger_cancels_the_draw() {
        let (mut s, cfg) = yumi();
        run(&mut s, &cfg, 36, HOLD);
        let ev = run(&mut s, &cfg, 1, PowerInput { can_act: false, ..HOLD });
        assert_eq!(ev, vec![PowerEvent::DrawCancel { tap: false }], "no arrow from a guard that went up");
        assert_eq!(s.ammo, 3);
    }

    #[test]
    fn tags_throw_then_detonate_by_hand_once_the_hands_are_empty() {
        let cfg = PowerCfg::default();
        let mut s = PowerState::default();
        assert!(s.grant(PowerKind::Tag, 0, 0.0, &cfg));
        assert_eq!(s.ammo, 2);
        let press = PowerInput { fire: true, ..IDLE };
        assert_eq!(run(&mut s, &cfg, 1, press), vec![PowerEvent::TagThrow]);
        assert!(s.used_fire);
        assert!(run(&mut s, &cfg, 1, press).is_empty(), "inside the cooldown the press is swallowed");
        assert!(s.used_fire);
        run(&mut s, &cfg, 30, IDLE);
        assert_eq!(run(&mut s, &cfg, 1, press), vec![PowerEvent::TagThrow]);
        assert_eq!(s.ammo, 0);
        run(&mut s, &cfg, 30, IDLE);
        assert!(run(&mut s, &cfg, 1, press).is_empty() && !s.used_fire, "nothing armed: the press is a star throw again");
        assert!(run(&mut s, &cfg, 1, PowerInput { live_tags: 1, ..press }).is_empty() && s.used_fire, "a live but unarmed tag swallows the press");
        assert_eq!(run(&mut s, &cfg, 1, PowerInput { live_tags: 1, armed_tags: 1, ..press }), vec![PowerEvent::TagDetonate]);
        assert!(s.spent_out(0) && !s.held());
    }

    #[test]
    fn an_occupied_hand_takes_nothing() {
        let (mut s, cfg) = yumi();
        assert!(!s.grant(PowerKind::Tag, 0, 0.0, &cfg));
        assert_eq!(s.kind, PowerKind::Yumi);
    }

    #[test]
    fn segments_and_blast_numbers() {
        let (d, s) = seg_seg([0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [5.0, 1.0, -3.0], [5.0, 1.0, 3.0]);
        assert!((d - 1.0).abs() < 1e-5 && (s - 0.5).abs() < 1e-5);
        assert!((point_seg([0.0; 3], [10.0, 0.0, 0.0], [12.0, 0.0, 0.0]) - 2.0).abs() < 1e-5);
        let c = TagCfg::default();
        assert_eq!(blast_damage(&c, 0.0), 50.0);
        assert!((blast_damage(&c, 4.0) - 20.0).abs() < 1e-4, "40 % at the edge");
        assert_eq!(blast_damage(&c, 4.1), 0.0);
        let v = blast_push(&c, [0.0, 0.0, 0.0], [0.0; 3], [2.0, 0.0, 0.0]);
        assert!(v[0] > 8.0 && v[2].abs() < 1e-4 && v[1] > 0.0, "pushed away from the blast: {v:?}");
        let on_me = blast_push(&c, [0.0, 0.0, 0.0], [0.0, 0.0, 5.0], [0.0, 0.0, 0.1]);
        assert!(on_me[2] < 0.0, "a tag on the body pushes away from the thrower: {on_me:?}");
    }
}
