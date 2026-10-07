# Audio

Sound is on by default (cargo feature `audio`, a default feature of `crates/shinobi-game`). All effects are synthesized
at startup (`src/synth.rs`), no asset files. No audio device: the game logs "No audio device found" and runs silent.

- Silent build: `cargo build --no-default-features --features wayland` (in `crates/shinobi-game`).
- Linux build needs the alsa dev headers (`libasound2-dev`).
- Console (`T` / chat key): `/vol 0-100`, `/mute`, `/unmute`.

(Buses, music folder and `/music` commands are documented below as they land.)
