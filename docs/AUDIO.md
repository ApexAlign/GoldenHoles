# Audio

Sound is on by default (cargo feature `audio`, a default feature of `crates/shinobi-game`). Every effect is
synthesized at startup (`src/synth.rs`), no asset files. No audio device: the game logs "audio: no output device;
running silent" and carries on (nothing is queued, no panic). `SHINOBI_NOAUDIO=1` forces silence.

- Silent build: `cargo build --no-default-features --features wayland` (in `crates/shinobi-game`).
- Linux build needs the alsa dev headers (`libasound2-dev`).
- Layers: `synth.rs` (sounds), `audio.rs` (`Cuer`: game state -> cues; playback backend), `mixer.rs` (buses, limiter,
  ducking, falloff, playlist; all pure and unit-tested).

## Mixer

Buses: master (`/vol`), sfx, music, ambient (wind + hum), plus mute. Saved to `audio.cfg` next to `settings.cfg`
(`~/.config/shinobi/` on Linux) one second after the last change.

A limiter stops spam clipping: each sound has a minimum gap (footsteps 0.12 s, hit ticks 0.04 s, lockout buzz 0.3 s...)
and all sounds share a loudness budget (3.0 "full-volume cues" refilling at 8/s). Past the budget, new cues are turned
down, then dropped; kills, hurt, parry, death, respawn and stingers are never dropped. Big events duck the music
(kill, execute, slam, death, match end) and it recovers over 1.2 s.

Other players' sounds (shuriken, ropes, footsteps, deaths) are placed in 3D and attenuated by distance
(`mixer::falloff`: loud inside ~7 m, gone at 70 m). Wind scales with your height (street whisper, rooftop roar).

## Console commands (chat/console key)

| command | effect |
| --- | --- |
| `/vol 0-100`, `/mute`, `/unmute` | master |
| `/sfx 0-100` | effects bus |
| `/ambient 0-100` | wind / hum bus |
| `/music vol 0-100` | music bus |
| `/music next` | crossfade to the next track |
| `/music off`, `/music on` | stop / resume music |

## Music slot

Drop tracks into a `music/` folder. Searched in order, first folder holding any track wins:

1. `$SHINOBI_MUSIC_DIR`
2. `music/` next to the binary (and the repo `music/` above `target/release/`)
3. `music/` in the working directory (repo root, or `crates/shinobi-game/../../music`)

Plays shuffled and endless (every track once per pass, no immediate repeat; one track loops), crossfading 4 s
between tracks (the next track starts 4 s before the end when the length is known). A broken file is skipped with a
warning. An empty or missing folder is fine: no music, no error.

Formats: `.wav` (16-bit PCM and others hound reads) works in the default build. `.ogg` / `.mp3` / `.flac` are picked up
but only decode when bevy is built with its `vorbis` / `mp3` / `symphonia-flac` features. They are not enabled because
they pull extra crates (lewton / minimp3 / symphonia) that are not in the offline cache. To enable them, add
`"bevy/vorbis", "bevy/mp3"` to a new feature in `crates/shinobi-game/Cargo.toml` with network access, or just convert
tracks to wav (`ffmpeg -i in.mp3 -ar 44100 music/in.wav`). Unsupported files are skipped with a log line.

## What plays when

Shuriken throw / ricochet / hit tick / kill chime / headshot tick; star lockout buzz and refill ping; shield block
clang, parry bell, low-guard beep; katana light / heavy (charge whine, release) / air slam / whirl / counter zap /
execute chime / wall slash / yank; grapple fire, attach thunk, rope pulse and reel loop, release; wall-run loop;
footsteps (stride-timed to speed, left/right pitch); jump, double jump, land, hard land, slide, dash; damage taken
and low-health heartbeat; kill streak (rises in pitch); void fall whoosh, death sting, respawn chime; match start,
victory and defeat stingers; UI click on menu / console open and close; ambient wind and hum.
Footsteps do not vary by surface yet: the client does not expose the surface under the player.
