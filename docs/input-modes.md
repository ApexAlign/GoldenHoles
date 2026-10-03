# Input modes (Cam & Mov) and the hit reaction camera

`shinobi_client::input_mode` (pure logic, tested) decides what the game may read while an interactive HUD overlay is up;
`shinobi-game/src/play.rs` applies it each frame (`ModeRes`, a Bevy resource). Pointer lock (click to grab, Esc = pause,
focus loss) is unchanged and underneath it.

| mode | opens | move | look | actions | cursor |
|---|---|---|---|---|---|
| Play | default | yes | yes | yes | grabbed |
| Scoreboard | hold Tab | yes | no | no | free (hover works) |
| BuildRadial | hold N/M >= 0.35 s (`radial_enabled`, OFF until HUD has the menu) | yes | no | no | free |
| Console | backquote toggles | no | no | no | free |

- Entering a mode that blocks actions releases every held button; Console also zeroes the stick, so keys typed into the
  console never leak into the game.
- Esc closes Console / BuildRadial first (`consumed_escape`: it is not forwarded to the pointer lock); a second Esc is the
  pause menu as before. Losing the lock (pause, focus loss) resets everything to Play.
- A build tap still prints immediately; a hold >= `radial_hold_secs` additionally opens the radial menu (when enabled).
  Letting go sets `ModeOut.radial_commit` for one frame: commit the highlighted piece.
- HUD UI: read `ModeRes.out` (`Option<ModeOut>`: `mode`, `flags.cursor_free`, `radial_commit`) to draw the console / radial
  menu; set `ModeRes.machine.cfg.radial_enabled = true` when the radial menu exists. Text typed in the console is yours
  (read `KeyboardInput`/`Ime` only while `mode() == Console`).

## Katana hit reaction (camera)
`FpsCamera::hit_react(slash_right, killed, stop)`, called by `ClientGame` on a predicted `SlashHit`: `stop` seconds of tight
shake (the 60 ms hit-stop; the viewmodel freeze is Graphics'), then an 0.18 s spring-back. A kick against the blade
direction (3.5 cm / 2.5 deg roll / -2.5 deg FOV; kills 7 cm / 4 deg / -5 deg), all scaled by `feel.shake` (zero under reduced
motion). It is a view-frame TRANSLATION (`CameraPose.offset`, applied in `follow_camera`), roll and FOV only: yaw / pitch
are untouched, so the crosshair stays on the aim that goes out in intents (test: pose.yaw/pitch == aim through a hit).
