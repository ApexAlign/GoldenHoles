use bevy::prelude::*;

/// Despawns after `timer`; if `shrink`, thins out as it expires (neon streaks).
#[derive(Component)]
pub struct Fx {
    pub timer: Timer,
    pub shrink: bool,
    /// Scale at birth; shrink multiplies this (so stretched trail segments keep their length).
    pub base_scale: Vec3,
}

impl Fx {
    pub fn new(secs: f32, shrink: bool) -> Self {
        Self::with_scale(secs, shrink, Vec3::ONE)
    }

    pub fn with_scale(secs: f32, shrink: bool, base_scale: Vec3) -> Self {
        Self { timer: Timer::from_seconds(secs, TimerMode::Once), shrink, base_scale }
    }
}

/// Visual spin about local Y (shuriken disc mesh).
#[derive(Component)]
pub struct Spin(pub f32);

pub fn tick_fx(
    mut commands: Commands,
    time: Res<Time>,
    mut fx: Query<(Entity, &mut Fx, &mut Transform)>,
    mut spin: Query<(&Spin, &mut Transform), Without<Fx>>,
) {
    for (e, mut fx, mut tf) in &mut fx {
        fx.timer.tick(time.delta());
        if fx.timer.finished() {
            commands.entity(e).despawn_recursive();
        } else if fx.shrink {
            let t = 1.0 - fx.timer.fraction();
            tf.scale = fx.base_scale * Vec3::new(1.0, t, t);
        }
    }
    for (s, mut tf) in &mut spin {
        tf.rotate_local_y(s.0 * time.delta_secs());
    }
}
