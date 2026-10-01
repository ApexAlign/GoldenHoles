# shinobi-game: platform feasibility (wasm32 and Android)

Research only. Nothing was installed, no Cargo.toml or lockfile was changed. Facts below come from the local
bevy 0.19.1 registry source, this machine's toolchain and the repo; anything I could not verify offline is marked
**(verify)**.

## Verdict

| target | feasible? | main cost |
|---|---|---|
| Android (aarch64) | Yes, medium effort | NDK + Java/Gradle packaging, a `cdylib` entry point, bevy `android-game-activity` |
| Browser (wasm32, WebGPU/WebGL2) | Yes, larger effort | **transport rewrite** (no UDP in browsers) and a server-side gateway |
| iOS | Not assessed beyond "needs macOS/Xcode" | n/a here |

The architecture already helps: `ClientGame`, `NetClient`, all sim/proto code and every draw-data module are
sans-IO and render-agnostic. The only platform seams are `shinobi-client/src/udp_game.rs`
(`UdpGame`, `std::net::UdpSocket`) and `udp_session.rs` (same), plus `shinobi-game` itself.

## What this machine has today

- `rustc 1.98.1`. Installed rustup targets: **only `x86_64-unknown-linux-gnu`**.
- Installable (not installed): `wasm32-unknown-unknown`, `wasm32-unknown-emscripten`, `wasm32-wasip1*`,
  `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`, `aarch64-apple-ios*`.
- Missing tools: `wasm-bindgen` CLI, `trunk`, `wasm-opt`, `wasm-pack`, `cargo-ndk`.
- Android: `~/Android/Sdk` exists (platforms, build-tools, platform-tools, emulator) but has **no `ndk/`**.
  `java` is OpenJDK 25.0.4.1 **(verify Gradle/AGP version compatibility)**. `ANDROID_NDK_HOME` unset.
- `shinobi-game/Cargo.lock` already contains `wasm-bindgen`, `web-sys`, `js-sys`, `web-time`,
  `console_error_panic_hook`, `android-activity`, `ndk`, `ndk-sys`, `bevy_android`, `winit` and
  `wgpu 29.0.4`: cargo locks every target's dependencies, so **no new crates are needed for bevy itself** on
  either target; they just are not downloaded/compiled until you build for that target
  (`cargo fetch --target ...` would pull their sources: a network step).

## Current bevy feature set and what each target changes

Now: `bevy_winit, x11, bevy_window, bevy_render, bevy_core_pipeline, bevy_pbr, bevy_log, multi_threaded,
bevy_post_process` (all bevy `=0.19.1`).

Feature names exist in bevy 0.19.1's `Cargo.toml`: `webgl2`, `webgpu`, `web`, `android-game-activity`,
`android-native-activity`, `touch`, `std`, `async_executor`.

### Browser (`wasm32-unknown-unknown`)
- **Drop**: `multi_threaded` (no threads without SharedArrayBuffer + atomics build), `x11`.
- **Add**: `web` and one of `webgl2` / `webgpu` (or both, wgpu picks at runtime **(verify)**). Keep `bevy_winit`
  (it is the web window/canvas backend).
- WebGL2 limits to respect: no compute, no vertex-stage storage buffers. The shipped bevy path is fine
  (grid, sky, CRT, walls and HUD use uniforms + dynamic meshes only). The standalone `lightwall.wgsl` and
  `ribbon_fx.wgsl` use storage buffers and would need the CPU-strip path (`trail::wall_strip`, already used).
  HDR render targets need `EXT_color_buffer_float` on WebGL2 **(verify on target browsers)**.
- Tooling: `rustup target add wasm32-unknown-unknown`, `wasm-bindgen-cli` (version must match the lockfile's
  `wasm-bindgen`), `trunk` or a manual `wasm-bindgen --target web`, optionally `wasm-opt`.
- Build flags: `getrandom` is `0.4.3` in the lock; wasm usually needs its `wasm_js` backend feature and a
  `--cfg getrandom_backend="wasm_js"` rustflag **(verify which crates pull it and whether the feature is already enabled by bevy)**.
- Code to change in this repo (all small):
  1. A `Transport` seam replacing `UdpSocket` in `udp_game.rs` (send/recv datagram, non-blocking). Native keeps UDP.
  2. `main.rs`: no `fn main` args on web (use URL query for `--server/--tier`), `--shot*` modes are native only.
  3. `std::time::Instant` is not used in client/sim/proto/game (checked), so no `web-time` shim is needed there.
- Determinism risk: server (native) and client (wasm) both run `rapier3d =0.22` for prediction. Float results can
  differ across platforms unless rapier's `enhanced-determinism` feature is on **everywhere** (changes the server
  too, Engine/Cam-Mov decision). Reconciliation tolerates small drift but corrections would be more frequent.
- Size/perf: single-threaded wasm; expect a large `.wasm` (tens of MB before `wasm-opt`) **(verify)**.

### Android (`aarch64-linux-android`)
- **Drop**: `x11`, `multi_threaded` is fine on Android (keep).
- **Add**: `android-game-activity` (bevy's recommended; `android-native-activity` is the alternative).
  `touch` support is already in `bevy_input`; `TouchInput` is what `mobile.rs::touch_input` consumes.
- Packaging: `shinobi-game` is a binary today. Android needs a `cdylib` target exposing
  `#[bevy_main]`/`android_main`, plus a Gradle/Java shell (GameActivity needs the Java side).
  Tooling: `rustup target add aarch64-linux-android`, `cargo-ndk` (or `xbuild`/`cargo-apk` for
  NativeActivity), an **NDK** (not present: install through `sdkmanager "ndk;<version>"`), set `ANDROID_NDK_HOME`.
- Existing UDP transport works unchanged on Android (needs the INTERNET permission in the manifest).
- Mobile-specific app code already in place: quality tiers (`--tier`), dynamic resolution, touch overlay.
  Still to add: auto-pick tier from `AdapterInfo`, pause/resume + focus-loss handling (`TouchSession::cancel_all`
  exists), safe-area insets for the HUD, and moving `LevelDesc` loading off a hard-coded floor.

## Network transport for browsers

Browsers have **no UDP and no raw sockets**. Options (server stays UDP; a gateway translates):

| option | semantics | fit | server-side crates (new) |
|---|---|---|---|
| WebTransport datagrams (HTTP/3 over QUIC) | unreliable, unordered datagrams | **best fit**: same model as UDP, no head-of-line blocking | `wtransport` (or `quinn` + `h3`), `tokio`, `rustls`; dev certs via `rcgen` |
| WebSocket (binary frames) | reliable, ordered (TCP) | works everywhere, but one lost packet stalls all later ones (bad for 60 Hz inputs/snapshots) | `tokio-tungstenite` (or `axum` ws), `tokio` |
| WebRTC data channels (unreliable mode) | unreliable | most complex (ICE/STUN/signalling) | `webrtc` crates; probably not worth it |

- Gateway design (no change to `shinobi-server` or the wire format): a separate process accepts a browser session,
  opens one UDP socket per session to the game server (so the server sees a distinct source address, which its
  per-peer logic needs), and forwards datagrams both ways unchanged. Client packets are <= 512 B and
  snapshots are well under an MTU, so they fit WebTransport datagrams **(verify the browser datagram size limit)**.
- WebTransport needs TLS. Dev: self-signed cert + the browser API's `serverCertificateHashes` (short-lived certs,
  Chromium-family **(verify)**). Prod: a real certificate. Browser support for WebTransport varies; Safari is the
  one to **verify** before committing; WebSocket is the fallback.
- Browser client side: `web-sys` is already in the lockfile; it needs the `WebSocket` / `WebTransport*` feature
  flags enabled **(verify names)**, i.e. a feature change on an existing crate, not a new crate.
- WebSocket-over-TCP would need a jitter-tolerant tweak: the client already tolerates loss via input redundancy,
  but head-of-line blocking shows up as latency spikes, not loss, so prediction/interp delay should be raised.

## What would need approval (new targets/crates), in one list

1. rustup targets: `wasm32-unknown-unknown`, `aarch64-linux-android`.
2. Tools: `wasm-bindgen-cli` (pinned to lock version), `trunk` (optional), `wasm-opt` (optional), `cargo-ndk`,
   Android NDK (+ Gradle project for GameActivity).
3. bevy feature flags (same `=0.19.1`): `web` + `webgl2`/`webgpu`; `android-game-activity`.
4. Server-side gateway crates: `tokio` + (`wtransport` or `tokio-tungstenite`) + `rcgen`/`rustls` for dev certs.
5. Possibly `getrandom` `wasm_js` feature and a rustflag; rapier `enhanced-determinism` (affects the server too).
