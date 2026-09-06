Reference for the `/rs` Cargo workspace extending the root `/CLAUDE.md` and `/CONTRIBUTING.md`.

# Crates
We want the library to be modular, so it's split into a bunch of crates.

**Core**
- `moq-net` (lib): the core wire layer. Negotiates `moq-lite` or IETF `moq-transport`. The public API uses the Broadcast/Track/Group/Frame model and the Producer/Consumer split (see below). Generic over `web_transport_trait::Session`.
- `moq-native` (lib): native connection helpers. Configures multiple QUIC backends (Quinn/Quiche/Noq/Iroh), WebTransport, WebSocket, TCP/TLS, Unix sockets, etc. 
- `kio` (lib): "easy async". Use `poll_*` methods to drive async I/O, with `async` helpers for `Future`-based async.

**Generic**
- `moq-json` (lib): generic JSON publishing over a track, in two modules. `snapshot` is lossy latest-value. `stream` is a lossless append-log (every record preserved in order).
- `moq-flate` (lib): group-scoped DEFLATE primitive. `Encoder`/`Decoder` turn a stream of payloads into self-delimited sync-flushed frames sharing one window (RFC 7692 marker trick), so similar frames compress against the earlier ones.
- `moq-stats` (lib): stats publishing and consumption over `moq-net` + `moq-json`. 

**Media**
- `hang` (lib): media layer on `moq-net`. `catalog/` is the JSON manifest; `container/` is the frame format (timestamp + codec payload).
- `moq-loc` (lib): LOC (Low Overhead Container) wire frame codec. Alternative to hang's container.
- `moq-msf` (lib): IETF MSF/CMSF catalog types. Alternative to hang's catalog.
- `moq-mux` (lib): the transmuxer. File/stream formats (`container/`: fmp4, flv, mkv, ts, loc) and codec parsers (`codec/`: h264, h265, av1, vp8/9, opus, aac, ...) <-> hang broadcasts.
- `moq-audio` (lib): native audio capture/encoding/decoding/rendering.
- `moq-video` (lib): native video capture/encoding/decoding/rendering, replacing ffmpeg. Focuses on hardware acceleration and zero-copy.
- `moq-transcode` (lib): just-in-time live transcoding of media broadcasts. 

**Apps / Gateways**
- `moq-relay` (lib+bin): clusterable, media-agnostic relay. 
- `moq-cli` (bin, `moq`): the unified media router (`moq <MoQ side> <import|export> <endpoint>`.
- `moq-rtc` (lib): WebRTC (WHIP/WHEP) gateway. Bridges browser WebRTC ingest/playback to MoQ broadcasts.
- `moq-rtmp` (lib): RTMP / enhanced-RTMP gateway 
- `moq-srt` (lib): bidirectional SRT gateway.
- `moq-hls` (lib): HLS / LL-HLS gateway.
- `moq-bench` (bin): load generator.
- `moq-boy` (bin): crowd-controlled Game Boy emulator publisher.
- `moq-token` (lib+bin): JWT auth token generation and validation.

**Bindings**
- `moq-ffi` (cdylib+staticlib): UniFFI bindings (Python/Swift/Kotlin/Go/Dart).
- `libmoq` (staticlib): C bindings.
- `moq-gst` (cdylib): GStreamer plugin.
- `moq-wasm` (cdylib+rlib): browser/WASM bindings.

When you change `moq-ffi`'s surface, mirror it in `libmoq` and the language wrappers.

## Producer / Consumer Model

Many crates are built on a split-handle pattern: a `Producer` writes, one or more `Consumer`s read, state is shared via `kio`. 
This split handle naturally fans out to any number of consumers.

## Async / poll plumbing
Two ways to drive things, both backed by `kio`:

- `poll_*` functions that take a `&kio::Waiter` and return `Poll<...>`, drivable from any executor or synchronously. Very similar to `Future` but cleans up Wakers on Drop
- `async fn` runs on any executor, although some methods currently require a tokio runtime.

Follow the root `poll_*` conventions:

# Guidelines
- Prefer `Ok(x?)` over `.map_err(Into::into)`.
- Use `ready!(...)` instead of `Poll::Pending => return Poll::Pending`.
- `Poll<Result<T, E>>` supports `?`; use `ready!(poll())?` or `match poll()?`.
- Prefer `kio` over tokio sync primitives.
- `thiserror` with `#[from]` for libraries, always `#[non_exhaustive]` on public error enums. 
- `anyhow` (with `.context("...")`, not `.map_err(|_| anyhow!())`) for binaries..
- Make terminal operations consume `self` (e.g. `fn close(self)`) so use-after-close can't even be written.
- Rely on `Drop` instead of letting the user forget a `close` call.
- Take in references to data that the callee only reads, and return owned values.
- Prefer `if let Some(v) = x { ... }` / `let Some(v) = x else { ... };` over a `match` whose only job is to bind the inner value. Keep `match` when both arms do real work.
- Async tests that depend on time call `tokio::time::pause()` first so timers fire instantly and deterministically.
- Workspace members live in the root `Cargo.toml` (`[workspace]`).
- Shared versions/paths are pinned under `[workspace.dependencies]`; new crates should add their dep there and reference it via `{ workspace = true }`.
- Prefer public modules with short names. ex. `broadcast::Consumer` instead of `BroadcastConsumer`

# Semver
Tips to avoid unintentional semver bumps:

- Use `#[error(transparent)]` + `#[from]` for wrapped foreign errors.
- `#[non_exhaustive]` for structs that will realistically gain fields. Provide a builder.
- Append new variants to the end of a public fieldless enum that uses implicit discriminants.
