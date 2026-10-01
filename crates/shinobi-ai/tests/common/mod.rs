#![allow(dead_code)]
use shinobi_ai::fsm::Phase;
use shinobi_ai::{Archetype, BotHost, BotHostCfg, BotSkill};
use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::character::Cfg;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

pub fn flat_server(spawns: Vec<[f32; 3]>) -> Server {
    let mut s = Server::new(Cfg::default());
    s.add_static_box([0.0, -0.5, 0.0], [300.0, 0.5, 300.0]);
    s.set_spawns(spawns);
    s
}

pub fn push_human(s: &mut Server, id: u16, it: PlayerIntent) {
    let tick = s.tick;
    let p = s.players.iter_mut().find(|p| p.id == id).unwrap();
    p.queue.push_packet(&ClientPacket { intents: vec![PlayerIntent { tick, ..it }], ..Default::default() }, tick);
}

pub fn idle(yaw: f32) -> PlayerIntent {
    PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
}

pub fn pos(s: &Server, id: u16) -> [f32; 3] {
    let p = s.players.iter().find(|p| p.id == id).unwrap();
    p.pos(&s.world)
}

pub fn damage_to(events: &[PvpEvent], victim: u16) -> f32 {
    events
        .iter()
        .map(|e| match *e {
            PvpEvent::Hit { victim: v, amount, .. } | PvpEvent::HazardHit { victim: v, amount, .. } if v == victim => amount,
            _ => 0.0,
        })
        .sum()
}

pub fn host(seed: u64) -> BotHost {
    BotHost::new(BotHostCfg { seed, ..Default::default() })
}

pub fn phase_name(h: &BotHost, id: u16) -> &'static str {
    h.bot(id).map_or("gone", |b| b.phase.name())
}

pub fn is_strike(h: &BotHost, id: u16) -> bool {
    matches!(h.bot(id).map(|b| b.phase), Some(Phase::Strike { .. }))
}

pub fn spawn_bot(h: &mut BotHost, s: &mut Server, id: u16, at: [f32; 3], a: Archetype, k: BotSkill) {
    s.set_spawns(vec![at]);
    assert!(h.add_bot(s, id, a, k));
    let _ = Buttons(0);
}

/// Intents are pushed by the host; the harness only advances one tick.
pub fn step(h: &mut BotHost, s: &mut Server) -> shinobi_sim::tick::FrameReport {
    h.tick(s)
}
