//! What the renderer and the HUD see of the power weapons (docs/powerups.md): the weapon in hand (predicted), and the arrows,
//! fuda tags and drawn bows other players' snapshots carry. Pure data + the snapshot decoding, tested here.

use shinobi_proto::quant::{dq_pos3, dq_vel};
use shinobi_proto::snapshot::{entity_class, EntityClass, EntityId, Snapshot, ARROW_REFLECTED, STUCK_NORMAL_SPEED, TAG_ARMED, TAG_FLYING, TAG_ON_PLAYER};
use shinobi_sim::power::{PowerCfg, PowerKind, PowerState};

/// The weapon in the local player's hands, as the predictor has it.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PowerView {
    pub kind: PowerKind,
    pub ammo: u8,
    /// Arrows / tags a fresh pickup holds (for the pips).
    pub ammo_max: u8,
    /// Carry time left, 0..=1 of a fresh pickup's.
    pub left: f32,
    /// Seconds of carry left.
    pub left_secs: f32,
    /// Draw progress 0..=1 and whether releasing now looses the arrow.
    pub draw: f32,
    pub full: bool,
    /// Between arrows / throws (can't draw / throw yet), seconds left.
    pub cooldown: f32,
    /// Own fuda tags out there and how many of them are armed (a press with empty hands sets those off).
    pub live_tags: u32,
    pub armed_tags: u32,
}

impl PowerView {
    pub fn of(s: &PowerState, cfg: &PowerCfg, live_tags: u32, armed_tags: u32) -> Option<Self> {
        if !s.held() && live_tags == 0 {
            return None;
        }
        let (max, carry) = cfg.fresh(s.kind);
        Some(Self {
            kind: s.kind,
            ammo: s.ammo,
            ammo_max: max,
            left: if carry > 0.0 { (s.left / carry).clamp(0.0, 1.0) } else { 0.0 },
            left_secs: s.left.max(0.0),
            draw: s.draw_frac(cfg),
            full: s.full(cfg),
            cooldown: s.cooldown,
            live_tags,
            armed_tags,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrowObj {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub owner: u16,
    pub reflected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagObj {
    pub pos: [f32; 3],
    /// Flight velocity (a tag in the air; zero once it is stuck).
    pub vel: [f32; 3],
    /// Surface normal (a tag stuck to a player: pointing back at where it came from).
    pub normal: [f32; 3],
    pub owner: u16,
    pub flying: bool,
    /// Stuck to this player.
    pub on_player: Option<u16>,
    pub armed: bool,
    /// Fuse left 0..=1 (1 = just stuck).
    pub fuse_left: f32,
}

/// A bow held out by another player (or us, via the server): `pos` is the eye, `dir` the aim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawObj {
    pub pos: [f32; 3],
    pub dir: [f32; 3],
    pub owner: u16,
    pub frac: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerWorld {
    pub arrows: Vec<ArrowObj>,
    pub tags: Vec<TagObj>,
    pub draws: Vec<DrawObj>,
}

impl PowerWorld {
    /// Every power entity of a snapshot. `pos_of` may replace the snapshot position (interpolation at the render tick).
    pub fn of(snap: &Snapshot, pos_of: impl Fn(EntityId) -> Option<[f32; 3]>) -> Self {
        let mut w = Self::default();
        for (id, st) in &snap.entities {
            let class = entity_class(*id);
            if !matches!(class, EntityClass::Arrow | EntityClass::Tag | EntityClass::Draw) {
                continue;
            }
            let pos = pos_of(*id).unwrap_or_else(|| dq_pos3(st.pos));
            let v = [dq_vel(st.vel[0]), dq_vel(st.vel[1]), dq_vel(st.vel[2])];
            match class {
                EntityClass::Arrow => w.arrows.push(ArrowObj { pos, vel: v, owner: st.yaw, reflected: st.flags & ARROW_REFLECTED != 0 }),
                EntityClass::Tag => {
                    let flying = st.flags & TAG_FLYING != 0;
                    let on_player = (st.flags & TAG_ON_PLAYER != 0).then_some(st.vel[0] as u16);
                    let normal = if flying || on_player.is_some() { [0.0, 1.0, 0.0] } else { [v[0] / STUCK_NORMAL_SPEED, v[1] / STUCK_NORMAL_SPEED, v[2] / STUCK_NORMAL_SPEED] };
                    w.tags.push(TagObj { pos, vel: if flying { v } else { [0.0; 3] }, normal, owner: st.yaw, flying, on_player, armed: st.flags & TAG_ARMED != 0, fuse_left: (st.fx & 15) as f32 / 15.0 });
                }
                _ => w.draws.push(DrawObj { pos, dir: [v[0] / STUCK_NORMAL_SPEED, v[1] / STUCK_NORMAL_SPEED, v[2] / STUCK_NORMAL_SPEED], owner: st.yaw, frac: (st.fx & 15) as f32 / 15.0 }),
            }
        }
        w
    }

    /// (live, armed) tags thrown by `me`.
    pub fn tags_of(&self, me: u16) -> (u32, u32) {
        let mine = self.tags.iter().filter(|t| t.owner == me);
        (mine.clone().count() as u32, mine.filter(|t| t.armed).count() as u32)
    }

    /// The tag stuck to `me` that goes off first (smallest fuse left), if any: the "TAG ON YOU" warning.
    pub fn tag_on(&self, me: u16) -> Option<&TagObj> {
        self.tags.iter().filter(|t| t.on_player == Some(me)).min_by(|a, b| a.fuse_left.total_cmp(&b.fuse_left))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::quant::{q_pos3, q_vel};
    use shinobi_proto::snapshot::{EntityState, ARROW_ID_BASE, DRAW_ID_BASE, TAG_ID_BASE};

    fn st(pos: [f32; 3], vel: [f32; 3], yaw: u16, flags: u8, fx: u8) -> EntityState {
        EntityState { pos: q_pos3(pos), vel: [q_vel(vel[0]), q_vel(vel[1]), q_vel(vel[2])], yaw, flags, fx, ..Default::default() }
    }

    #[test]
    fn the_snapshot_entities_decode_into_arrows_tags_and_bows() {
        let n = STUCK_NORMAL_SPEED;
        let snap = Snapshot {
            tick: 5,
            entities: vec![
                (1, st([0.0; 3], [0.0; 3], 0, 0, 0)),
                (ARROW_ID_BASE + 3, st([1.0, 2.0, 3.0], [0.0, 0.0, -90.0], 7, ARROW_REFLECTED, 0)),
                (TAG_ID_BASE + 1, st([4.0, 0.0, 0.0], [0.0, n, 0.0], 7, TAG_ARMED, 8)),
                (TAG_ID_BASE + 2, st([5.0, 1.0, 0.0], [0.0; 3], 9, TAG_ON_PLAYER, 15)),
                (DRAW_ID_BASE + 7, st([0.0, 1.5, 0.0], [0.0, 0.0, -n], 7, 0, 15)),
            ],
        };
        let mut snap = snap;
        snap.entities[3].1.vel = [1, 0, 0]; // victim id 1 rides in vel[0] (raw)
        let w = PowerWorld::of(&snap, |_| None);
        assert_eq!((w.arrows.len(), w.tags.len(), w.draws.len()), (1, 2, 1));
        assert!(w.arrows[0].reflected && w.arrows[0].owner == 7 && (w.arrows[0].vel[2] + 90.0).abs() < 0.1);
        assert!(w.tags[0].armed && (w.tags[0].normal[1] - 1.0).abs() < 0.05 && (w.tags[0].fuse_left - 8.0 / 15.0).abs() < 1e-4);
        assert_eq!(w.tags[1].on_player, Some(1));
        assert_eq!(w.tag_on(1).map(|t| t.owner), Some(9));
        assert_eq!(w.tags_of(7), (1, 1));
        assert!((w.draws[0].dir[2] + 1.0).abs() < 0.05 && w.draws[0].frac == 1.0);
    }

    #[test]
    fn the_view_shows_nothing_with_empty_hands_and_no_tags_out() {
        let cfg = PowerCfg::default();
        assert!(PowerView::of(&PowerState::default(), &cfg, 0, 0).is_none());
        let mut s = PowerState::default();
        s.grant(PowerKind::Yumi, 0, 0.0, &cfg);
        let v = PowerView::of(&s, &cfg, 0, 0).unwrap();
        assert_eq!((v.kind, v.ammo, v.ammo_max), (PowerKind::Yumi, 3, 3));
        assert!(v.left > 0.99 && v.draw == 0.0);
        // a spent bag with a tag still counting down keeps a view (the detonate hint)
        let v = PowerView::of(&PowerState::default(), &cfg, 1, 0).unwrap();
        assert_eq!(v.live_tags, 1);
    }
}
