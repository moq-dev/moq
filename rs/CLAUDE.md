# rs/CLAUDE.md

Reference for the `/rs` Cargo workspace. Universal rules (writing style, no em dashes, Branch Targeting, Cross-Package Sync, Public API Scrutiny, Refactor As You Go, AI Attribution) live in the root `/CLAUDE.md` and are not repeated here.

Workspace members live in the root `Cargo.toml` (`[workspace]`). `rust-version = "1.85"`, edition 2024. Shared versions/paths are pinned under `[workspace.dependencies]`; new crates should add their dep there and reference it via `{ workspace = true }`.

## Crate Map

Layered roughly transport -> container/format -> media -> apps/bindings.

**Transport / protocol**

- `moq-net` (lib): the core wire layer. Negotiates `moq-lite` or IETF `moq-transport`. Owns the Broadcast/Track/Group/Frame model and the Producer/Consumer split (see below). Generic over `web_transport_trait::Session` (no concrete QUIC dep). Submodules are private; the public surface is re-exported flat from the crate root.
- `moq-native` (lib): native connection helpers. `ClientConfig`/`ServerConfig` wrap QUIC backends (Quinn/Quiche/Noq/Iroh), WebTransport, WebSocket, TCP (qmux), Unix sockets, TLS, cert hot-reload, logging, jemalloc. Re-exports `moq_net`. Example: `examples/clock.rs`.
- `kio` (lib): "easy async". `Producer<T>`/`Consumer<T>` shared-state channels with `Waiter`-based notification, built on `std::task::Waker`, no runtime dependency. Underpins all the `poll_*` plumbing in moq-net and moq-mux. `src/producer.rs`, `src/consumer.rs`, `src/waiter.rs`.

**Container / catalog formats** (standalone specs, mostly no moq-\* deps, reused by moq-mux)

- `hang` (lib): media layer on `moq-net`. `catalog/` is the JSON manifest (`Catalog`, root.rs); `container/` is the frame format (timestamp + codec payload, `container::Frame`).
- `moq-loc` (lib): LOC (Low Overhead Container) wire frame codec. Top-level `encode`/`decode` + `Frame`. QUIC varints, property KVPs.
- `moq-msf` (lib): IETF MSF/CMSF catalog types (`Catalog`, `Track`, `Packaging`, `Role`). serde JSON. Alternative to hang's catalog.
- `moq-json` (lib): generic snapshot/delta value publishing over a track using RFC 7396 JSON Merge Patch. `Producer<T>`/`Consumer<T>`, `Guard<T>` (RAII edit). Late joiners reconstruct from snapshot + deltas. DEFLATE via `moq-flate`.
- `moq-flate` (lib): group-scoped DEFLATE primitive (no moq deps). `Encoder`/`Decoder` turn a stream of payloads into self-delimited sync-flushed frames sharing one window (RFC 7692 marker trick), so similar frames compress against the earlier ones. Used by `moq-json`; reusable by any framed stream.

**Media bridge / codecs**

- `moq-mux` (lib): the conversion layer. File/stream formats (`container/`: fmp4, flv, mkv, ts, loc) and codec parsers (`codec/`: h264, h265, av1, vp8/9, opus, aac, ...) <-> hang broadcasts. `Container` trait + generic `Producer<C>`/`Consumer<C>`. Dual catalog (`catalog::hang`, `catalog::msf`).
- `moq-audio` (lib): native PCM <-> Opus (`unsafe-libopus`). `AudioProducer`/`AudioConsumer`, `Encoder`/`Decoder`, `AudioFormat`. Optional `capture` feature (cpal microphone), `resample`.
- `moq-video` (lib): native webcam capture + H.264 via `ffmpeg-next`. `capture::Config`, `encode::{Encoder, Producer, publish_capture}`. ffmpeg types kept out of the public signature (see `error/`).

**Apps / binaries**

- `moq-relay` (lib+bin): clusterable, media-agnostic relay. axum HTTP API, JWT auth, WebSocket fallback, clustering. Config/TOML merge pattern lives here (see below).
- `moq-rtc` (lib+bin): WebRTC (WHIP/WHEP) gateway. Bridges browser WebRTC ingest/playback to MoQ broadcasts (ICE/DTLS, A/V sync, NACK).
- `moq-cli` (lib+bin, `moq`): serve/accept/publish/subscribe; stdin/stdout media piping.
- `moq-rtc` (lib+bin): WebRTC (WHIP/WHEP) gateway. Bridges browser WebRTC ingest/playback to MoQ broadcasts (str0m ICE/DTLS, A/V sync, NACK). Embeddable lib (`default-features = false`) + standalone binary (`server` feature).
- `moq-bench` (bin): relay load generator. `JoinSet`-spawned staggered connections, rand sampling.
- `moq-boy` (bin): crowd-controlled Game Boy emulator publisher (blocking emulator thread + async monitor tasks).
- `moq-token` (lib) / `moq-token-cli` (bin): JWT auth. `Claims`, `Algorithm`, `KeyType` (EC/RSA/OCT/OKP), JWKS. CLI does generate/sign/verify.

**Bindings**

- `moq-ffi` (cdylib+staticlib): UniFFI bindings (Python/Swift/Kotlin/Go). Proc-macro based (`uniffi::setup_scaffolding!("moq")`, `#[uniffi::Object]`/`#[uniffi::export]`), no `.udl`. Exposes `Moq*Producer`/`Moq*Consumer`, `MoqError` (`#[uniffi(flat_error)]`).
- `libmoq` (staticlib): C bindings. `cbindgen` `build.rs` emits `moq.h` + pkg-config. `extern "C"` over opaque handles; dedicated tokio runtime thread (`LazyLock`).
- `moq-gst` (cdylib): GStreamer plugin. `gst::plugin_define!`, `moqsrc`/`moqsink` elements bridging to a background tokio task.

When you change `moq-ffi`'s surface, mirror it in `libmoq` and the language wrappers (see the Cross-Package Sync table in root).

## Producer / Consumer Model (moq-net)

The whole stack is built on a split-handle pattern: a `Producer` writes, one or more `Consumer`s read, state is shared via `kio`. This recurs in moq-net, moq-mux, moq-json.

- Broadcast: `BroadcastProducer` / `BroadcastConsumer` / `BroadcastDynamic` (`model/broadcast.rs:74,370,216`).
- Track: `TrackProducer` / `TrackConsumer` / `TrackWeak` (`model/track.rs:206,459,425`).
- Group: `GroupProducer` / `GroupConsumer` (`model/group.rs:140,286`). Consumers `clone()` for fanout.
- Frame: `FrameProducer` (impls `BufMut`) / `FrameConsumer` (`model/frame.rs:162,317`).
- Origin: `OriginProducer` / `OriginConsumer` (`model/origin.rs`).

## Async / poll plumbing

Two ways to drive things, both backed by `kio`:

- `async fn` (requires an active tokio runtime; awaiting outside one may panic, see `moq-net/src/lib.rs:42`).
- `poll_*` counterparts that take a `&kio::Waiter` and return `Poll<...>`, drivable from any executor or synchronously (`kio` is built on `std::task::Waker`). The `async` method usually just wraps the `poll_*` one via `kio::wait`. Example pair: `TrackConsumer::poll_recv_group` / `recv_group` (`moq-net/src/model/track.rs:502,518`).

Follow the root `poll_*` conventions: collapse `Poll::Pending => Poll::Pending` with `ready!(...)`, and prefer `Ok(x?)` over `.map_err(Into::into)` so a fallible poll reads `let v = ready!(inner.poll_next(cx))?;`. Representative `ready!` sites: `moq-mux/src/container/consumer.rs:201`, `moq-net/src/model/group.rs`.

## Version matching

`moq_net::Version` is `#[non_exhaustive]`, splitting `Lite(lite::Version)` and `Ietf(ietf::Version)` (`version.rs:47`). When matching on a `Version` (or the inner draft enums), default to the **newest** draft so future versions fall forward; list older versions explicitly:

```rust
match version {
    Version::Draft14 | Version::Draft15 | Version::Draft16 => { /* old behavior */ }
    _ => { /* newest / draft-17+ behavior */ }
}
```

Negotiation: `version::NEGOTIATED` lists SETUP-negotiated versions in preference order; newer drafts negotiate via dedicated ALPNs (`version::ALPNS`). The version-to-behavior dispatch lives in `setup.rs:73` (`SetupVersion::from_version`).

## Rust conventions

- **Prefer `kio` over tokio sync primitives**: reach for `kio::Producer`/`Consumer` (and the `poll_*` plumbing) instead of `tokio::sync` channels or `watch`. A `tokio::sync::watch` (or a channel) carrying a single value is a code smell. `kio` ties into the runtime-free `poll_*` model and avoids a hard runtime dependency.
- **Errors**: `thiserror` with `#[from]` for libraries, `anyhow` (with `.context("...")`, not `.map_err(|_| anyhow!())`) for binaries. Always `#[non_exhaustive]` on public error enums (e.g. `moq-net/src/error.rs:6`, `moq-ffi/src/error.rs:4`, `moq-loc/src/lib.rs:55`). Use `#[error(transparent)]` + `#[from]` for wrapped foreign errors (see `moq-token/src/error.rs`).
- **Config + TOML merge**: any `#[arg]` field on a TOML-loadable config must be `Option<T>`, never a bare `bool`/`String`/etc. The TOML->CLI merge re-applies clap defaults and silently clobbers TOML values for bare fields. See `moq-relay/src/config.rs` and its regression tests (`cli_does_not_clobber_toml_*`, around line 126); add such a test for any new flag.
- **Config structs**: `#[derive(Parser, Serialize, Deserialize)]` with `#[serde(deny_unknown_fields, default)]`, clap `#[arg(long, env = "MOQ_...")]`, nested configs via `#[command(flatten)]`, and an `.init()`/`.load()` method that produces the live object. Add `#[non_exhaustive]` + `Default`/constructor to **public-field** configs consumers build (per root Public API Scrutiny). A struct whose fields are all private and built through a builder (e.g. `select::Broadcast`) already blocks struct literals, so it doesn't need `#[non_exhaustive]`.
- **Unwrapping**: prefer `if let Some(v) = x { ... }` / `let Some(v) = x else { ... };` over a `match` whose only job is to bind the inner value. Keep `match` when both arms do real work.
- **Naming**: role-based module + short unprefixed type (`encode::Encoder`, `capture::Config`), not `EncoderConfig`/`CameraConfig`. Re-export flat to avoid stutter (`mod encoder` private, `pub use encoder::Encoder`).

## Binary setup

Binaries are `#[tokio::main] async fn main() -> anyhow::Result<()>`. Install the rustls crypto provider before anything TLS:

```rust
rustls::crypto::aws_lc_rs::default_provider().install_default().expect("crypto provider");
```

Then `Config::load()?` (initializes tracing), build clients/servers via `.init()`, and run an event loop with `tokio::select!`. See `moq-relay/src/main.rs`, `moq-bench/src/main.rs`.

## Testing

- `just check` runs all tests + lint; `just fix` auto-fixes formatting/lint. `cargo test -p <crate>` for one crate.
- Rust tests are `#[cfg(test)] mod tests` inline in the source file.
- Async tests that depend on time call `tokio::time::pause()` first so timers fire instantly and deterministically (e.g. `moq-net/src/model/origin.rs:1099`).
- Config-merge regressions belong next to the config (`moq-relay/src/config.rs::tests`); they serialize env mutation with a lock since clap reads env.
