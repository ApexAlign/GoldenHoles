# Aegis shield: Kinetic Battery and `core_vent`

Status: implemented in the simulation, server and owner wire (protocol v31).

## Contract

Every player owns the Aegis shield and the Kinetic Battery from spawn. There is no loadout and no pickup that
unlocks either part. The Aegis block is the existing `block.rs` guard. The Battery is passive: it has no new
button, never changes movement, and is server-authoritative.

The Battery is a value in `0..=1000` fixed units (`0.0..=1.0` on the HUD). It fills from three sources:

| Source | Fill | Notes |
|---|---:|---|
| A blocked enemy star | `shield.blocked_star_charge` (0.08) | `Guard::Blocked` against `DamageKind::Shuriken`, or a `power_pass` arrow hit already marked `guarded` |
| A successful parry | `shield.parry_charge` (0.25) | Any weapon parried by `block::guard_vs` (star, arrow or katana) |
| Standing in a `core_vent` zone | `1000 / fill_ticks` per tick | Authored per zone; progress persists while outside |

A blocked katana cut does **not** charge the Battery. An unguarded hit, a hazard, a tag blast and a self-hit
never charge it either. Charges saturate at `1000`.

## Aura

When an alive player's Battery reaches `1000`, the server consumes the whole Battery and starts the Kinetic Aura
on that player at the end of the tick. The aura lasts `shield.aura_s` (5 s = 300 ticks):

- Only unguarded `DamageKind::Slash` damage from that player is multiplied by `shield.aura_damage_mult` (1.2).
- Blocked, parried, star, arrow, tag, hazard, obstacle and enemy damage are unchanged.
- Movement, guard, reach, swing speed, prediction and lock-on are unchanged.
- The aura cannot retrigger while it is active. The Battery can still fill during it; if it is full when the aura
  expires, a new aura starts on that expiry tick.
- Death cancels the aura. The stored Battery and the `core_vent` cooldown survive death; a spawn with a full
  Battery therefore starts an aura.

This is deliberately a melee reward, not a defensive stat. The counterplay is spacing and the existing shield
counterplay: a blocked cut does not feed the attacker, and a full Battery aura is wasted if its owner cannot
close the distance inside 5 s.

## `core_vent`

A `zone` with the tag `core_vent` is a neutral charging volume. A living player whose capsule overlaps the AABB
charges while the zone is not on their cooldown:

```text
zone cx cy cz hx hy hz core_vent cooldown_ticks=1200 fill_ticks=90 reveal_ticks=300
```

- `fill_ticks` is the number of ticks of continuous occupation needed for a complete fill. It defaults to `90`.
- `cooldown_ticks` starts when a fill completes. During it the zone gives that player no charge. It defaults to
  `0`.
- Leaving the zone pauses the fill; the partial progress (and therefore the visible Battery) is kept. Re-entry
  continues from it.
- `reveal_ticks` (default `300`) is the price of a fill: when a fill completes, the filler is revealed to every
  player for that many ticks. Revealed players show on everyone's radar at any range (pinned to the rim when out
  of range) and are named in a cyan `VENT:` line under the BOUNTY callout, with a compass caret. The filler sees
  `VENT REVEALED YOU`. Death ends the reveal early; the Battery does not depend on it.
- While a player's own cooldown runs, their view of the vent dims (down to a quarter brightness, back to full when
  ready). Other players' cooldowns are not shown.
- The zone is neutral: any player can use it, and multiple players standing in it charge independently.
- While the match is frozen (warmup countdown or intermission), the zone does not charge.

## Wire

The Battery and aura are owner-only: the server's `OwnState` (protocol v31) carries

- `battery_q: u16` (10 bits, `0..=1000`);
- `aura_ticks_left: u16` (16 bits, relative to the snapshot tick);
- `aura_total_ticks: u16` (16 bits, 0 when inactive).

The HUD reads these through `ClientGame` / `RenderView` (`shield_battery`, `shield_aura_ticks`,
`shield_aura_total`). The vent reveal is a broadcast reliable event (protocol v35, tag 33):
`VentReveal { tick, player, ticks }`, sent when a fill completes (`ticks = reveal_ticks`) and with `ticks = 0` when
the revealed player dies or leaves; natural expiry needs no event. Late joiners get the live reveals on join. The
owner's cooldown dim is derived client-side from its own last `VentReveal` tick plus the zone's `cooldown_ticks`.

Remote aura drawing is deferred to a visual pass: it would need a snapshot status bit or a
reliable event and is not part of this wire change. The owner sees the authoritative own-state values, and no
client prediction of damage or Battery exists.

## Balance keys

| Key | Default | Meaning |
|---|---:|---|
| `shield.blocked_star_charge` | 0.08 | Battery fraction gained by blocking a star or arrow |
| `shield.parry_charge` | 0.25 | Battery fraction gained by a successful parry |
| `shield.aura_s` | 5.0 | Aura duration in seconds |
| `shield.aura_damage_mult` | 1.2 | Unguarded katana damage multiplier while the aura is active |

Vent timing is level tuning (`fill_ticks`, `cooldown_ticks`), not a balance key.

## Tests

- Block a star and an arrow: charge exactly `blocked_star_charge`; block a katana: no charge.
- Parry a star / katana: charge exactly `parry_charge`.
- `core_vent`: fill over `fill_ticks`, persist across an exit, respect the per-player cooldown, and charge
  multiple players independently.
- Aura: starts only at full, consumes the Battery, lasts exactly 300 ticks, multiplies only unguarded slash
  damage, expires, does not retrigger while active, and retriggers on expiry when the Battery was refilled.
- Movement is bit-identical with an empty and a full Battery / active aura.
- `OwnState` roundtrip and size tests; a 1000-tick two-server replay produces identical shield and movement
  state.
