//! Aim feel against a MOVING target: a scripted strafer at 8 / 16 / 30 m and a human-ish aimer (reaction lag, bounded
//! turn rate, hand jitter), with and without the look assist. Reports time-to-lock and the share of time the
//! target stays locked, and asserts the assist helps and a mid-range target is held most of the time.
//!   cargo test --release -p shinobi-sim --test aim_feel -- --nocapture
use shinobi_sim::balance::Balance;
use shinobi_sim::character::{PhysicsWorld, GROUP_ENEMY};
use shinobi_sim::combat::aim_assist::{angles_of, best_target, AimAssist, AssistInput, LockCfg, LockPhase, LockState};
use shinobi_sim::combat::GROUP_WORLD;
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;
const SECS: usize = 8;

struct Rng(u64);
impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

struct Outcome {
    time_to_lock: f32,
    held: f32,
    /// Share of ticks after the first lock that the crosshair itself is on the body (a hit without any assist).
    on_target: f32,
}

/// `strafe`: target lateral speed m/s (direction flips every `flip` s); `bob`: vertical amplitude (wall-run / jump).
fn run(dist: f32, strafe: f32, bob: f32, assist: bool, touch: bool, seed: u64) -> Outcome {
    let b = Balance::default();
    let lock_cfg = LockCfg { lock_secs: b.combat.lock_secs, pull_min: b.combat.lock_pull_min, break_secs: b.combat.lock_break_secs, blend_secs: b.combat.lock_blend_secs };
    let cone = b.combat.shuriken.assist_cone_deg;
    let mut w = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 1.0, 0.0]).build());
    let enemy = w.colliders.insert(ColliderBuilder::cuboid(0.4, 0.9, 0.4).translation(vector![0.0, 1.0, -dist]).collision_groups(InteractionGroups::new(GROUP_ENEMY, Group::ALL)).user_data(7).build());
    let _ = GROUP_WORLD;
    let eye = vector![0.0, 1.6, 0.0];
    let mut rng = Rng(seed ^ 0xA5A5);
    let (mut yaw, mut pitch) = (0.35f32, 0.0f32); // looking ~20 degrees off: the first lock has to be earned
    let mut lock = LockState::default();
    let mut aa = AimAssist::default();
    let mut hist: Vec<Vector<Real>> = Vec::new();
    let (mut first_lock, mut held_ticks, mut counted, mut on_ticks) = (None::<f32>, 0usize, 0usize, 0usize);
    let mut jitter = (0.0f32, 0.0f32);
    let mut x = -0.45 * strafe; // start left of centre so the mover crosses the crosshair
    for t in 0..SECS * 60 {
        let time = t as f32 * DT;
        // scripted mover: strafes sideways, flipping direction every 0.9 s, optional vertical bob
        let dir = if ((time / 0.9) as usize) % 2 == 0 { 1.0 } else { -1.0 };
        x += dir * strafe * DT;
        let pos = vector![x, 1.0 + bob * (time * 3.0).sin().abs(), -dist];
        w.colliders[enemy].set_translation(pos);
        w.queries.update(&w.colliders);
        hist.push(pos);

        // the human reacts ~0.13 s late, turns at most 7 rad/s, hand jitter ~0.5 deg wandering
        // (a person extrapolates a little: half the velocity over the lag)
        let old = hist[t.saturating_sub(8)];
        let seen = old + (pos - old) * 0.5 + vector![0.0, 0.2, 0.0];
        let (wy, wp) = angles_of(seen - eye);
        let (dyaw, dpitch) = ((wy - yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI, wp - pitch);
        jitter.0 = jitter.0 * 0.9 + rng.unit() * 0.0015;
        jitter.1 = jitter.1 * 0.9 + rng.unit() * 0.0015;
        let cap = 7.0 * DT;
        let before = (yaw, pitch);
        let raw = (yaw + (dyaw * 0.3).clamp(-cap, cap) + jitter.0, pitch + (dpitch * 0.3).clamp(-cap, cap) + jitter.1);

        let input = (assist && lock.pull(&lock_cfg) > 0.0).then(|| AssistInput {
            eye,
            target: vector![lock.pos[0], lock.pos[1], lock.pos[2]],
            target_id: lock.target_id,
            pull: lock.pull(&lock_cfg),
            cone_deg: cone,
            touch,
            dt: DT,
        });
        let (ny, np) = aa.apply(&b.assist, input.as_ref(), before, raw);
        yaw = ny;
        pitch = np;

        let fwd = vector![-yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos()];
        let found = best_target(&w, eye, fwd, cone, 60.0, &[], body);
        lock.update(DT, &lock_cfg, found.as_ref());
        if lock.phase == LockPhase::Locked && first_lock.is_none() {
            first_lock = Some(time);
        }
        if let Some(t0) = first_lock {
            if time >= t0 {
                counted += 1;
                held_ticks += lock.holding() as usize;
                // does the crosshair ray pass through the 0.8 x 1.8 m body box?
                let k = dist / -fwd.z.min(-1e-3);
                let (hx, hy) = (eye.x + fwd.x * k, eye.y + fwd.y * k);
                on_ticks += ((hx - pos.x).abs() < 0.4 && (hy - pos.y).abs() < 0.9) as usize;
            }
        }
    }
    Outcome { time_to_lock: first_lock.unwrap_or(f32::INFINITY), held: if counted == 0 { 0.0 } else { held_ticks as f32 / counted as f32 }, on_target: if counted == 0 { 0.0 } else { on_ticks as f32 / counted as f32 } }
}

fn avg(dist: f32, strafe: f32, bob: f32, assist: bool, touch: bool) -> (f32, f32, f32) {
    let (mut ttl, mut held, mut on) = (0.0, 0.0, 0.0);
    let n = 20;
    for seed in 0..n {
        let o = run(dist, strafe, bob, assist, touch, seed as u64);
        ttl += o.time_to_lock.min(SECS as f32);
        held += o.held;
        on += o.on_target;
    }
    (ttl / n as f32, held / n as f32, on / n as f32)
}

#[test]
fn lock_time_hold_and_crosshair_on_target_against_a_strafing_target() {
    let (mut helped, mut total) = (0, 0);
    let mut held_min = 1.0f32;
    for (name, strafe, bob) in [("strafe 5 m/s", 5.0, 0.0), ("strafe 8 m/s", 8.0, 0.0), ("wall-run 6 m/s + bob", 6.0, 1.5)] {
        for dist in [8.0f32, 16.0, 30.0] {
            let (t0, h0, o0) = avg(dist, strafe, bob, false, false);
            let (t1, h1, o1) = avg(dist, strafe, bob, true, false);
            let (_, h2, o2) = avg(dist, strafe, bob, true, true);
            println!(
                "{name:20} {dist:>4} m | none: lock {t0:.2}s hold {:>3.0}% on-body {:>3.0}% | mouse: lock {t1:.2}s hold {:>3.0}% on-body {:>3.0}% | touch: hold {:>3.0}% on-body {:>3.0}%",
                h0 * 100.0,
                o0 * 100.0,
                h1 * 100.0,
                o1 * 100.0,
                h2 * 100.0,
                o2 * 100.0
            );
            total += 1;
            helped += (o1 >= o0 - 0.01) as usize;
            held_min = held_min.min(h1);
        }
    }
    assert!(helped == total, "the look assist must not lower the on-body share (helped in {helped}/{total})");
    assert!(held_min >= 0.85, "lock held >= 85% of the time in every scenario, worst {held_min:.2}");
}
