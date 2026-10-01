use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use super::{
    katana::Katana,
    shuriken::ShurikenLauncher,
    viewmodel::{FpsCamera, SlashEvent},
    CombatAssets, DamageEvent, Enemy, Player,
};

/// Everything a weapon may touch when it fires. Weapons stay plain Rust
/// objects; the ECS plumbing lives here.
pub struct FireCtx<'a, 'w, 's> {
    pub commands: &'a mut Commands<'w, 's>,
    pub assets: &'a CombatAssets,
    pub rapier: &'a RapierContext,
    pub enemies: &'a Query<'w, 's, &'static GlobalTransform, With<Enemy>>,
    pub damage: &'a mut EventWriter<'w, DamageEvent>,
    pub slash: &'a mut EventWriter<'w, SlashEvent>,
    pub owner: Entity,
    pub origin: Vec3,
    pub rotation: Quat,
    pub aim: Dir3,
    pub impulse: &'a mut ExternalImpulse,
}

/// Modular weapon. Implement this, box it into a `Loadout` slot.
pub trait Weapon: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Seconds between uses.
    fn cooldown(&self) -> f32;
    /// Fire once. Return `false` to refuse (no cooldown consumed).
    fn fire(&mut self, ctx: &mut FireCtx) -> bool;
}

#[derive(Clone, Copy)]
pub enum Trigger {
    Mouse(MouseButton),
    Key(KeyCode),
}

pub struct WeaponSlot {
    pub weapon: Box<dyn Weapon>,
    pub trigger: Trigger,
    pub cooldown_left: f32,
}

#[derive(Component, Default)]
pub struct Loadout {
    pub slots: Vec<WeaponSlot>,
}

impl Loadout {
    pub fn with(mut self, trigger: Trigger, weapon: impl Weapon) -> Self {
        self.slots.push(WeaponSlot { weapon: Box::new(weapon), trigger, cooldown_left: 0.0 });
        self
    }

    /// LMB = neon shurikens, RMB = cyber-katana dash slice.
    pub fn ninja() -> Self {
        Self::default()
            .with(Trigger::Mouse(MouseButton::Left), ShurikenLauncher::default())
            .with(Trigger::Mouse(MouseButton::Right), Katana::default())
    }
}

pub fn tick_cooldowns(time: Res<Time>, mut loadouts: Query<&mut Loadout>) {
    for mut l in &mut loadouts {
        for s in &mut l.slots {
            s.cooldown_left = (s.cooldown_left - time.delta_secs()).max(0.0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn weapon_input(
    mut commands: Commands,
    assets: Option<Res<CombatAssets>>,
    rapier: Res<RapierContext>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    enemies: Query<&GlobalTransform, With<Enemy>>,
    mut damage: EventWriter<DamageEvent>,
    mut slash: EventWriter<SlashEvent>,
    cameras: Query<&GlobalTransform, With<FpsCamera>>,
    mut players: Query<(Entity, &GlobalTransform, &mut Loadout, &mut ExternalImpulse), With<Player>>,
) {
    let Some(assets) = assets else { return };
    for (owner, gtf, mut loadout, mut impulse) in &mut players {
        // FPS: aim and muzzle come from the eye, not the body.
        let eye = cameras.get_single().unwrap_or(gtf);
        let (_, rotation, origin) = eye.to_scale_rotation_translation();
        let aim = eye.forward();
        for slot in &mut loadout.slots {
            let pressed = match slot.trigger {
                Trigger::Mouse(b) => mouse.just_pressed(b),
                Trigger::Key(k) => keys.just_pressed(k),
            };
            if !pressed || slot.cooldown_left > 0.0 {
                continue;
            }
            let mut ctx = FireCtx {
                commands: &mut commands,
                assets: &assets,
                rapier: &rapier,
                enemies: &enemies,
                damage: &mut damage,
                slash: &mut slash,
                owner,
                origin,
                rotation,
                aim,
                impulse: &mut impulse,
            };
            if slot.weapon.fire(&mut ctx) {
                slot.cooldown_left = slot.weapon.cooldown();
            }
        }
    }
}
