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
- `moq-json` (lib): generic JSON publishing over a track, in two modules. `snapshot` is lossy latest-value (RFC 7396 merge-patch deltas; consumers only get the most recent value; `Producer<T>`/`Consumer<T>`, `Guard<T>` RAII edit); `stream` is a lossless append-log (every record preserved in order). DEFLATE via `moq-flate`.
- `moq-flate` (lib): group-scoped DEFLATE primitive (no moq deps). `Encoder`/`Decoder` turn a stream of payloads into self-delimited sync-flushed frames sharing one window (RFC 7692 marker trick), so similar frames compress against the earlier ones. Used by `moq-json`; reusable by any framed stream.

**Media bridge / codecs**

- `moq-mux` (lib): the conversion layer. File/stream formats (`container/`: fmp4, flv, mkv, ts, loc) and codec parsers (`codec/`: h264, h265, av1, vp8/9, opus, aac, ...) <-> hang broadcasts. `Container` trait + generic `Producer<C>`/`Consumer<C>`. Dual catalog (`catalog::hang`, `catalog::msf`).
- `moq-audio` (lib): native PCM <-> Opus (`unsafe-libopus`). `AudioProducer`/`AudioConsumer`, `Encoder`/`Decoder`, `AudioFormat`. Optional `capture` feature (cpal microphone), `resample`.
- `moq-video` (lib): native webcam capture + H.264 via `ffmpeg-next`. `capture::Config`, `encode::{Encoder, Producer, publish_capture}`. ffmpeg types kept out of the public signature (see `error/`).

**Apps / binaries**

- `moq-relay` (lib+bin): clusterable, media-agnostic relay. axum HTTP API, JWT auth, WebSocket fallback, clustering. Config/TOML merge pattern lives here (see below).
- `moq-cli` (lib+bin, `moq`): serve/accept/publish/subscribe + `hls`; stdin/stdout media piping. The CLI surface for the gateway library crates below lives here (a `moq-cli`-driven command-line interface).
- `moq-rtc` (lib): WebRTC (WHIP/WHEP) gateway. Bridges browser WebRTC ingest/playback to MoQ broadcasts (str0m ICE/DTLS, A/V sync, NACK). Embed via its axum routers / `Client`.
- `moq-rtmp` (lib): RTMP / enhanced-RTMP gateway (ingest + egress, `rml_rtmp`, FLV via `moq-mux`). RTMPS (rustls + tokio-rustls) is the optional `tls` feature.
- `moq-srt` (lib): bidirectional SRT gateway (MPEG-TS via `srt-tokio` + `moq-mux`).
- `moq-bench` (bin): relay load generator. `JoinSet`-spawned staggered connections, rand sampling.
- `moq-boy` (bin): crowd-controlled Game Boy emulator publisher (blocking emulator thread + async monitor tasks).
- `moq-token` (lib) / `moq-token` (bin from the `moq-token-cli` crate): JWT auth. `Claims`, `Algorithm`, `KeyType` (EC/RSA/OCT/OKP), JWKS. CLI does generate/sign/verify.

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
- **Config structs**: `#[derive(Parser, Serialize, Deserialize)]` with `#[serde(deny_unknown_fields, default)]`, clap `#[arg(long, env = "MOQ_...")]`, nested configs via `#[command(flatten)]`, and an `.init()`/`.load()` method that produces the live object. See the `#[non_exhaustive]` conventions below for whether the struct gets the attribute and/or a builder.
- **`#[non_exhaustive]` (future-proofing additions, per root Public API Scrutiny)**: the attribute's only job is to keep *adding* a field/variant from being a semver-breaking change. It is not a reflexive default.
  - *Structs*: add it to one that will probably grow with additive, defaultable fields (the classic `Config`), paired with `Default`/a constructor so callers build via `default()`/`new()` + field set (not a struct literal), and prefer adding a field to it over adding a positional parameter. Skip it on a struct that won't grow, or where a new field would *change behavior* rather than default to a no-op. There the addition should be a deliberate breaking change, not one the attribute waves through.
  - *Enums*: add it to a public enum that may gain variants so external `match`es keep compiling; always on public error enums (see Errors above).
  - *Builders* (private fields + chained `.with_x()` setters) are the orthogonal construction-ergonomics layer: reach for one when a struct has a lot of optional knobs, or is `#[non_exhaustive]` and you want construction to stay clean as fields get added (e.g. `select::Broadcast`).
- **Make misuse unrepresentable in the type system** (root Public API Scrutiny): make terminal operations consume `self` (e.g. `fn close(self)`) so use-after-close can't even be written, rather than `&mut self` plus a `closed` flag. Return owned handles whose `Drop` runs the cleanup instead of asking callers to remember a teardown call.
- **Unwrapping**: prefer `if let Some(v) = x { ... }` / `let Some(v) = x else { ... };` over a `match` whose only job is to bind the inner value. Keep `match` when both arms do real work.
- **Naming / namespacing**: name by role, not by today's only implementation (`capture::Config`, `publish_capture`, not `CameraConfig`/`publish_camera`), so a second implementation slots in without a rename; don't bundle generic options under a specific-case name. Split a growing crate into role modules (`capture`, `encode`, `decode`) so each owns short, unprefixed names: the module supplies the prefix, so `encode::Config` beats `EncoderConfig` and `encode::Producer` beats `VideoProducer`. Don't nest a module whose name echoes its main type (`encode::encoder::Encoder` stutters): keep `mod encoder` private and re-export flat (`pub use encoder::{Encoder, Config}`) so it reads `encode::Encoder`.
- **Deprecation mechanics** (root Deprecation explains the why): a deprecated CLI flag stays a hidden alias (clap `alias = "..."`, or a separate `#[arg(..., hide = true)]` when it needs its own runtime deprecation warning); a deprecated public item gets `#[doc(hidden)]`. No `--help` entry and no "deprecated, use X" note in the doc comment, so it drops off docs.rs.

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
