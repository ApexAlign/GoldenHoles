use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};
use shinobi_proto::TICK_DT;
use shinobi_sim::character::{Cfg, CharacterSystem, MoveState, PhysicsWorld};
use shinobi_sim::features::Features;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::{to_player_input, Server};

const SPAWN: [f32; 3] = [10.0, 5.0, 4.7];
const LEVEL: &str = "box 0 -0.5 0 200 0.5 200\n\
    box 100 4 5.5 200 4 0.5\n\
    spawn 10 5 4.7\n\
    zone 50 4 4.5 50 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=10\n";

fn seed(ch: &mut CharacterSystem) {
    let cfg = ch.cfg;
    ch.vel = vector![cfg.wall_speed, 0.0, 0.0];
    ch.state = MoveState::WallRun {
        normal: vector![0.0, 0.0, -1.0],
        tangent: vector![1.0, 0.0, 0.0],
        time_left: 1000.0,
    };
}

fn intent(tick: u32) -> PlayerIntent {
    PlayerIntent {
        tick,
        buttons: Buttons(Buttons::JUMP_HELD | Buttons::WALL_CLING),
        move_x: 127,
        move_z: 0,
        aim_yaw: 0,
        aim_pitch: PITCH_LEVEL,
        ultimate: false,
        build: false,
        build_wall: false,
        build_ramp: false,
        zip: false,
        block: false,
        mark: false,
        star: 0,
        draw: false,
        cloak: false,
        grind: false,
        zoom: false,
        tether: false,
    }
}

fn bits(p: [f32; 3]) -> [u32; 3] {
    [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]
}

struct Client {
    world: PhysicsWorld,
    features: Features,
    ch: CharacterSystem,
    now: f32,
}

impl Client {
    fn new() -> Self {
        let f = parse_level_file(LEVEL).unwrap();
        let mut world = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        f.level.insert_into(&mut world);
        let mut features = Features::default();
        features.load(f.features, &mut world);
        let cfg = Cfg::default();
        let body = world.bodies.insert(
            RigidBodyBuilder::kinematic_position_based()
                .translation(vector![SPAWN[0], SPAWN[1], SPAWN[2]])
                .build(),
        );
        world.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(),
            body,
            &mut world.bodies,
        );
        let mut ch = CharacterSystem::new(body, cfg);
        seed(&mut ch);
        Self {
            world,
            features,
            ch,
            now: 0.0,
        }
    }

    fn step(&mut self, it: &PlayerIntent) -> [f32; 3] {
        let p = self.world.bodies[self.ch.body].translation();
        let normal = self.ch.wall_normal().map(|n| [n.x, n.y, n.z]);
        self.ch.speed_lane = self.features.defs().speed_lane_for([p.x, p.y, p.z], normal);
        step_player(&mut self.ch, &to_player_input(it), &mut self.world, self.now, TICK_DT);
        self.now += TICK_DT;
        let t = self.world.bodies[self.ch.body].translation();
        [t.x, t.y, t.z]
    }
}

#[test]
fn circuit_speed_lane_is_bit_identical_for_1000_wall_run_ticks() {
    let f = parse_level_file(LEVEL).unwrap();
    let mut srv = Server::new(Cfg::default());
    f.level.apply_to_server(&mut srv);
    srv.set_spawns(f.level.spawns.clone());
    srv.set_level_features(f.features.clone());
    srv.add_player(1, SPAWN);
    seed(&mut srv.players[0].ch);

    let mut cli = Client::new();
    let base = srv.players[0].ch.cfg.wall_speed;
    let mut boosted = false;
    let mut max_speed = 0.0f32;
    for tick in 0..1000 {
        let it = intent(tick);
        let bytes = ClientPacket { intents: vec![it], ..Default::default() }.encode().unwrap();
        srv.ingest(1, &bytes);
        while srv.tick == tick {
            srv.frame(Duration::from_nanos(16_666_667));
        }
        assert_eq!(srv.players[0].guard.strikes, 0, "tick {tick}: server struck an honest client");

        let server = srv.players[0].pos(&srv.world);
        let client = cli.step(&it);
        assert_eq!(bits(server), bits(client), "tick {tick}: server {server:?} vs client {client:?}");
        assert!(matches!(srv.players[0].ch.state, MoveState::WallRun { .. }), "tick {tick}: {:?}", srv.players[0].ch.state);

        let speed = srv.players[0].ch.vel.x;
        max_speed = max_speed.max(speed);
        boosted |= speed > base * 1.10;
    }
    assert!(boosted, "the lane never boosted the wall run (max {max_speed})");
    assert!(max_speed <= base * 1.15 + 1e-3, "authored cap: {max_speed}");
    assert!(srv.players[0].ch.vel.x <= base * 1.02, "post-exit decay returns to base: {}", srv.players[0].ch.vel.x);
}
