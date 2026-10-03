# Predator lock-on, melee/shot feedback, cyber-computer HUD (owner request)

## 1. Predator-style lock-on (the aim assist you can SEE)
Three glowing laser dots in a triangle ("shoulder cannon") that find a target, spin, converge and snap on.
- States: SEARCH (none) -> ACQUIRE (a target is inside the assist cone with line of sight: three dots appear on the
  target's chest, wide and slowly rotating) -> LOCK (dots converge over `lock_secs` ~0.35 s into a tight triangle,
  snap + pulse, short rising tone later when audio exists) -> BREAK (target leaves cone / loses LOS: dots scatter and
  fade). Lock progress is 0..1; lock only forms for ENEMIES (never allies, never self).
- The assist uses exactly the same target the triangle shows: one shared function (`aim_assist::best_target`) used by
  the shuriken throw (already in `assisted_dir`), the katana lunge/miss-assist, the server and the client predictor.
  Strength can scale with lock progress (full pull only at LOCK).
- Visuals: triangle drawn in WORLD space at the target (depth-aware, additive HDR so bloom picks it up, colour = YOUR
  neon colour), plus a subtle shoulder-cannon emitter on the viewmodel with three thin beams to the dots while locked.
- Data contract: `RenderView.lock: Option<LockOn { target: EntityId, pos: [f32;3], progress: f32, locked: bool }>`,
  computed by ClientGame from predicted state each frame. HUD draws target brackets/distance/hp-bar from the same.

## 2. Melee feedback (katana)
- Magnetism: lunge bends toward the locked target (same cone logic), never through walls.
- Hit confirm: `CombatEvent::SlashHit { target, point, killed }` -> impact flash at `point`, slash-arc flare, 50-80 ms
  hit-stop (client-only: freeze viewmodel + camera shake, sim never pauses), directional camera kick, bigger on kill.
- Miss feedback: a clear whiff (dim arc, small recover) so a hit never looks like a miss.

## 3. Shot feedback
- Throw: muzzle flash + shoulder-cannon recoil; hit: marker (already), target flashes in the shooter's colour,
  dissolving "data fragment" burst on kill; ricochet hits get a distinct ring + chime tag (`kill_tags` already).

## 4. Cyber-computer HUD, interactive
- Look: terminal/OS style. Thin neon vector panels, scanlines, subtle glitch/flicker, monospace readouts,
  boot-up sequence on spawn ("LINK ESTABLISHED" etc.), data glyph rain in dead space, corner brackets.
- Live panels: speed / altitude / rope length / wall-run state, ammo+cooldown bars, health as a segmented bar,
  radar/minimap with enemy pings (only when firing or within range, no wallhack), compass strip, kill-feed as a
  scrolling log, leaderboard (hold Tab).
- Target HUD: brackets around the locked enemy + name/colour/hp/distance.
- Interactive: Tab = scoreboard with mouse hover; hold build key = radial build menu (N floor/ramp, M wall, size
  preview) selectable with the mouse; backtick = in-game console (kill log, `/sens`, `/fov`, `/colour`, `/help`);
  Esc = pause "terminal" menu with clickable settings. Cursor appears only in these modes.
- Rules: keep the crosshair clear and the centre readable. The old clear-periphery test may be relaxed for slim side
  panels, but update the test to the new layout (slim panels, no content over the centre cross).

## Ownership
- Combat: `aim_assist::best_target` + LockOn data + katana magnetism + SlashHit event + hit-stop timing values.
- Graphics: triangle lock visuals, shoulder emitter, impact/flash/data-fragment effects, glitch/scanline shader bits.
- HUD UI: cyber layout, panels, radar, target brackets, radial build menu, console, boot sequence, interactive modes.
- Cam & Mov: camera kick / hit-stop camera shake integration, input modes (cursor grab/release for menus).
- Network: nothing new on the wire unless Combat needs SlashHit replicated to attacker (reuse hit msg kinds).
