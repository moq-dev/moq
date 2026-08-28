# rs/CLAUDE.md

Reference for the `/rs` Cargo workspace. Universal rules (writing style, no em dashes, Root Cause First, Cross-Package Sync, Public API Scrutiny, Refactor As You Go) live in the root `/CLAUDE.md`; PR/commit/release mechanics live in `/CONTRIBUTING.md`. Neither is repeated here.

Workspace members live in the root `Cargo.toml` (`[workspace]`). `rust-version = "1.91"` (the library floor; `moq-relay` overrides to 1.95 for `sysinfo`), edition 2024. Shared versions/paths are pinned under `[workspace.dependencies]`; new crates should add their dep there and reference it via `{ workspace = true }`.

## Crate Map

Layered roughly transport -> container/format -> media -> apps/bindings.

**Transport / protocol**

- `moq-net` (lib): the core wire layer. Negotiates `moq-lite` or IETF `moq-transport`. Owns the Broadcast/Track/Group/Frame model and the Producer/Consumer split (see below). Generic over `web_transport_trait::Session` (no concrete QUIC dep). Each level of the hierarchy is a public role module that owns short names (`broadcast::Consumer`, `track::Producer`, `group::Info`, `frame::Producer`, `origin::Consumer`, `announce::Consumer`); origin + announce share one private implementation surfaced as two curated modules. Traffic counters for those levels live in the `stats` module (`stats::Registry` collects, `stats::Handle` is what a session bumps through); publishing them as broadcasts lives in `moq-stats`.
- `moq-tokio` (lib): native connection helpers. `ClientConfig`/`ServerConfig` wrap QUIC backends (Quinn/Quiche/Noq/Iroh), WebTransport, WebSocket, TCP (qmux), Unix sockets, TLS, cert hot-reload, logging, jemalloc. Re-exports `moq_net`, and `moq_sock::bind` as `moq_tokio::bind`. Example: `examples/clock.rs`.
- `moq-sock` (lib): socket and thread-per-core listener plumbing shared by `moq-tokio` and `moq-uring`. `bind` (dual-stack UDP/TCP, grown buffers, `SO_REUSEPORT`), `shard` (reuseport group formation with the port lock and bind probe, the cBPF connection-id steering filter, `Shard`, `cid_prefix`), `cpu` (core pinning). The group-formation invariants (bind in index order, never resize) live in its docs; each runtime's worker group is what holds them.
- `moq-uring` (lib, experimental): the Linux thread-per-core io\_uring runtime. `Worker` (one per pinned thread, Linux 6.12 floor, no fallback) owns the ring, a userspace timer heap (`Handle` implements `moq_net::Timers`), a local `!Send` task set, and futex-word parking; `udp::Socket` does multishot provided-buffer receive with `UDP_GRO` and pooled `UDP_SEGMENT` sends; `quic::Endpoint` stacks a sans-IO QUIC stack on that path, serving many connections per socket demuxed by connection id (dials included, ids rotated, unsupported versions negotiated); native peers speak raw QUIC (ALPN-negotiated) while browsers negotiate `h3` and `quic::web::Request` runs the HTTP/3 CONNECT handshake via `web-transport-proto`, with `quic::web::Session` as the runtime's one transport type covering both flavors (`Session::raw` wraps raw connections) and `Handle` as the `moq_net::Runtime`, so `connect_lite`/`accept_lite` run moq-lite sessions on the worker either way. The stack under `quic` is one of two cargo features, never both: `quiche` (default, BoringSSL) or `quinn` (quinn-proto over rustls, no second TLS stack to build); `quinn` wins if a build asks for both, and with neither the `quic` module is left out and the crate is the worker alone. moq-relay picks the same choice with `io-uring` (quiche) or `io-uring-quinn` (quinn), which share the private `_uring` feature that gates the listener itself. `just rs uring` runs both. Validated by a raw-quiche echo (`tests/echo.rs`), full moq-lite pub/sub sessions including two clients on one server socket (`tests/session.rs`), the endpoint mechanics (`tests/endpoint.rs`), a steered two-worker group (`tests/workers.rs`), and WebTransport interop against `web-transport-quinn` including moq-lite over it (`tests/web.rs`), all kernel-gated and all run against whichever backend is compiled, plus the `echo_quiche` ablation benchmark; `benches/udp_{tokio,uring}.rs` are the older disposable syscall matrices.
- `kio` (lib): "easy async". `Producer<T>`/`Consumer<T>` shared-state channels with `Waiter`-based notification, built on `std::task::Waker`, no runtime dependency. Underpins all the `poll_*` plumbing in moq-net and moq-mux. `src/producer.rs`, `src/consumer.rs`, `src/waiter.rs`. Implement `Pollable` (a `poll(&Waiter)` computation) and wrap it in `Pending` to get a `std::future::Future` (`src/pollable.rs`). Guard discipline: the synchronous methods (`write`, `poll*`) report closure as `Err(Ref)`, a live lock guard; the `async` ones report it as `kio::Closed` instead, since an `Err` held across a later `.await` would stall every other handle.

**Container / catalog formats** (standalone specs, mostly no moq-\* deps, reused by moq-mux)

- `hang` (lib): media layer on `moq-net`. `catalog/` is the JSON manifest (`Catalog`, root.rs); `container/` is the frame format (timestamp + codec payload, `container::Frame`).
- `moq-loc` (lib): LOC (Low Overhead Container) wire frame codec. Top-level `encode`/`decode` + `Frame`. QUIC varints, property KVPs.
- `moq-msf` (lib): IETF MSF/CMSF catalog types (`Catalog`, `Track`, `Packaging`, `Role`). serde JSON. Alternative to hang's catalog.
- `moq-json` (lib): generic JSON publishing over a track, in two modules. `snapshot` is lossy latest-value (RFC 7396 merge-patch deltas; consumers only get the most recent value; `Producer<T>`/`Consumer<T>`, `Guard<T>` RAII edit); `stream` is a lossless append-log (every record preserved in order). DEFLATE via `moq-flate`.
- `moq-flate` (lib): group-scoped DEFLATE primitive (no moq deps). `Encoder`/`Decoder` turn a stream of payloads into self-delimited sync-flushed frames sharing one window (RFC 7692 marker trick), so similar frames compress against the earlier ones. Used by `moq-json`; reusable by any framed stream.
- `moq-stats` (lib): stats publishing and consumption over `moq-net` + `moq-json`. `Producer` drains a `moq_net::stats::Registry` on an interval into per-node JSON tracks (plain `.json` plus compressed `.json.z` siblings); `Consumer` yields typed `TrafficFrame`/`SessionsFrame` off one stats broadcast; `parse_node_path` + track-name helpers cover the announce/track naming scheme. The relay's stats surface is this crate.

**Media bridge / codecs**

- `moq-mux` (lib): the conversion layer. File/stream formats (`container/`: fmp4, flv, mkv, ts, loc) and codec parsers (`codec/`: h264, h265, av1, vp8/9, opus, aac, ...) <-> hang broadcasts. `Container` trait + generic `Producer<C>`/`Consumer<C>`. Dual catalog (`catalog::hang`, `catalog::msf`).
- `moq-audio` (lib): native PCM <-> Opus/PCM (`unsafe-libopus`), plus AAC-LC decode (`symphonia-codec-aac`, the default-on `aac` feature) for broadcasts that arrived through a gateway. Shaped like `moq-video`: `capture::Config`, `encode::{Encoder, Producer, publish_capture}`, `decode::{Consumer, Decoder}`, plus root `Error`/`Format`/`Frame`. Playback is the extra role module `moq-video` has no counterpart for: `playback::{Engine, Config, Sink, Control, Input, Device, devices}`, where one `Engine` owns the output device and mixes up to 64 `Sink`s into it on a driver thread. `aec::{Canceller, Config}` closes the loop between the two: `Engine::canceller` taps the post-mix signal and `capture::Config::aec` subtracts it from the microphone (`sonora`, a pure-Rust WebRTC APM port), which is what a call on a laptop needs to not send itself back. Optional `capture` feature (cpal microphone, macOS system audio), `playback` feature (cpal output, `fixed-resample` ring buffers), and `aec` feature (implies both).
- `moq-video` (lib): native video capture, H.264/H.265 encode, and decode; no ffmpeg. Hardware backends (VideoToolbox / Media Foundation / NVENC / VAAPI / NVDEC) with openh264 as the software H.264 fallback; NVDEC frames stay in CUDA memory and feed NVENC zero-copy. `capture::Config`, `encode::{Encoder, Producer, publish_capture}`, `decode::{Consumer, Decoder}`, root `Error`/`Size`.
- `moq-transcode` (lib): just-in-time live transcoding of hang broadcasts. `Transcoder::new(source, output, config)` registers the output tracks synchronously (announce `output` after it) and `run` drives it: a derivative catalog (ladder rungs + relative refs to the source) plus each rung encoded only while subscribed/fetched, via `moq-video`. The free `run(source, output, config)` is the shorthand. `Transcoder::active` hands out `active::Consumer` cursors over which renditions are encoding, each `active::Rendition` counting the frames and bytes it produced for a caller that bills. Live rungs share one decode per source (the `feed` module); output groups mirror source group sequences 1:1. Also a moq-cli verb (`moq ... transcode`, feature-gated).

**Apps / binaries**

- `moq-relay` (lib+bin): clusterable, media-agnostic relay. axum HTTP API, JWT auth, WebSocket fallback, clustering. Config/TOML merge pattern lives here (see below).
- `moq-cli` (bin, `moq`): the unified media router (`moq <MoQ side> <import|export> <endpoint>`, plus the feature-gated `transcode` verb); stdin/stdout media piping. The CLI surface for the gateway library crates below lives here. `token`, `devices`, and `completion` are the local verbs: they run before any transport is bound and reject a MoQ side rather than ignoring it. Shell completion lives in `src/complete.rs`, which owns both the `__complete_word__` plumbing (the root grammar answers a cursor before the first `--`, the stage grammar after one) and the runtime completers that dial the relay `--connect` already names.
- `moq-rtc` (lib): WebRTC (WHIP/WHEP) gateway. Bridges browser WebRTC ingest/playback to MoQ broadcasts (str0m ICE/DTLS, A/V sync, NACK). Embeddable axum routers / `Client`; the CLI surface lives in `moq-cli`.
- `moq-rtmp` (lib): RTMP / enhanced-RTMP gateway (ingest + egress, `rml_rtmp`, FLV via `moq-mux`). RTMPS (rustls + tokio-rustls) is the optional `tls` feature.
- `moq-srt` (lib): bidirectional SRT gateway (MPEG-TS via `srt-tokio` + `moq-mux`).
- `moq-hls` (lib): HLS / LL-HLS gateway (import + export, playlists + fMP4 via `moq-mux`).
- `moq-bench` (bin): relay load generator. `JoinSet`-spawned staggered connections, rand sampling.
- `moq-boy` (bin): crowd-controlled Game Boy emulator publisher (blocking emulator thread + async monitor tasks).
- `moq-token` (lib): JWT auth. `Claims`, `Algorithm`, `KeyMaterial` (EC/RSA/OCT/OKP), JWKS. No CLI parser or anyhow: the command surface lives a layer up.
- `moq-token-cli` (lib+bin, `moq-token`): the generate/sign/verify commands, as `moq_token_cli::Args`. The `moq-token` binary and `moq token` (moq-cli) share one implementation. It's a lib so moq-cli can reuse it without pulling Usage and anyhow into the `moq-token` library's API.

**Bindings**

- `moq-ffi` (cdylib+staticlib): UniFFI bindings (Python/Swift/Kotlin/Go). Proc-macro based (`uniffi::setup_scaffolding!("moq")`, `#[uniffi::Object]`/`#[uniffi::export]`), no `.udl`. Exposes `Moq*Producer`/`Moq*Consumer`, `MoqError` (`#[uniffi(flat_error)]`).
- `libmoq` (staticlib): C bindings. `cbindgen` `build.rs` emits `moq.h` + pkg-config. `extern "C"` over opaque handles; dedicated tokio runtime thread (`LazyLock`).
- `moq-gst` (cdylib): GStreamer plugin. `gst::plugin_define!`, `moqsrc`/`moqsink` elements bridging to a background tokio task.
- `moq-wasm` (cdylib+rlib): browser/WASM bindings, `wasm-bindgen` over `moq-net`. Consumed by `js/wasm` (`@moq/wasm`); build via `just wasm`.

When you change `moq-ffi`'s surface, mirror it in `libmoq` and the language wrappers (see the Cross-Package Sync table in root).

## Producer / Consumer Model (moq-net)

The whole stack is built on a split-handle pattern: a `Producer` writes, one or more `Consumer`s read, state is shared via `kio`. This recurs in moq-net, moq-mux, moq-json.

Each level is a role module (`broadcast`, `track`, `group`, `frame`, `origin`, `announce`) owning short `Producer`/`Consumer` names:

- Broadcast: `broadcast::{Producer, Consumer, Dynamic}` (`model/broadcast.rs`).
- Track: `track::{Producer, Consumer, Subscriber, ...}` plus the `pub(crate)` `track::TrackWeak` (`model/track.rs`).
- Group: `group::{Producer, Consumer, Info}` (`model/group.rs`). Consumers `clone()` for fanout.
- Frame: `frame::Producer` / `frame::Consumer` (`model/frame.rs`).
- Origin: `origin::{Producer, Consumer}` for the broadcast set; `announce::{Producer, Consumer}` for (un)announce events. Both share the private `origin.rs` implementation (`mod origin_impl`), surfaced via `model/mod.rs`.

## Async / poll plumbing

Two ways to drive things, both backed by `kio`:

- `async fn` (runs on any executor; session timers come from the `moq_net::Runtime` passed at connect/accept, so nothing needs an ambient tokio runtime, see the Async section of `moq-net/src/lib.rs`). The model layer arms no timers at all: it reads the crate-internal clock (`moq-net/src/model/clock.rs`, real in production, frozen-until-advanced under `cfg(test)`) for passive stamps, and the origin driver takes a `moq_net::Timers` at `origin::Driver::run` for its wakeup deadlines. kio itself has no time module.
- `poll_*` counterparts that take a `&kio::Waiter` and return `Poll<...>`, drivable from any executor or synchronously (`kio` is built on `std::task::Waker`). The `async` method usually just wraps the `poll_*` one via `kio::wait`. Example pair: `track::Consumer::poll_recv_group` / `recv_group` (`moq-net/src/model/track.rs`).

Sessions run on an explicit runtime: `Client::connect` / `Server::accept` take a `moq_net::Runtime` (assoc `Transport` + `Timer`, a `spawn` for the session's protocol `runtime::Machine`, and the clock timers arm against) and return the plain `Session`; there is no ambient executor or thread-local anywhere. The `Session` is the handle, with the library's usual refcount lifecycle (clones share the connection, the transport closes when the last clone drops, `abort(err)` closes explicitly). Handle and machine are severed in both directions (`moq-net/src/session.rs`): **the machine holds no `Session` clone**, so the runtime running it never keeps the session alive, and **the `Session` holds no transport**, so the handle is unconditionally `Send + Sync` whatever transport the runtime drives (which is what lets moq-ffi and the relay ignore a pinned `!Send` transport). Everything transport-shaped on the handle is relayed through the machine's supervisor leg: `abort`/last-drop become a close request it executes, `closed()` awaits the terminal error it publishes, and `stats()` reads its latest sample (refreshed at 100ms while stats are read or bandwidth is consumed). The machine's `Send`-ness is structural, not per-target: the lite driver is a named machine generic over the transport, so `transport::poll::{Session, SendStream, RecvStream}` carry no thread-affinity bounds and a pinned `!Send` transport yields a `!Send` machine via the lite-only entries (`Client::connect_lite`, `Server::accept_lite`). The ietf driver is still one boxed future (`runtime::Protocol::Ietf`), so the full-version entries (`connect`, `accept`, `accept_request`) and every tokio-spawning `Runtime` impl require `transport::poll::Boxable` (the old `MaybeSend` bundle) until the ietf tree is de-futured. Each transport ships its runtime: `moq_tokio::runtime::Runtime<S>` (tokio spawn + tokio timers; `runtime::Inline` hands the machine back for callers that drive it in place), moq-wasm's browser runtime, and `moq_net::runtime::Test` (feature `test-runtime`) with a virtual clock for deterministic tests. moq-net's own unit tests use the `cfg(test)` `runtime::tokio_test::Tokio`, which keeps `tokio::time::pause()` semantics.

Follow the root `poll_*` conventions: collapse `Poll::Pending => Poll::Pending` with `ready!(...)`, and prefer `Ok(x?)` over `.map_err(Into::into)` so a fallible poll reads `let v = ready!(inner.poll_next(cx))?;`. Representative `ready!` sites: `moq-mux/src/container/consumer.rs`, `moq-net/src/model/group.rs`.

## Version matching

`moq_net::Version` is `#[non_exhaustive]`, splitting `Lite(lite::Version)` and `Ietf(ietf::Version)` (`version.rs`). The inner `lite::Version` / `ietf::Version` payloads are crate-private, so outside `moq-net` you branch on the accessors rather than on variants: `is_lite()` / `is_ietf()` for the protocol family, and `alpn()` / `code()` for the specific draft.

```rust
// Outside the crate: family first, then the ALPN string for a specific draft.
if version.is_lite() {
    // moq-lite behavior
} else {
    match version.alpn() {
        "moqt-15" | "moqt-16" => { /* old behavior */ }
        _ => { /* newest / draft-17+ behavior */ }
    }
}
```

Inside `moq-net`, match the inner draft enums directly. Either way, default to the **newest** draft so future versions fall forward, and list older versions explicitly:

```rust
match version {
    ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16 => { /* old behavior */ }
    _ => { /* newest / draft-17+ behavior */ }
}
```

Negotiation: `version::NEGOTIATED` lists SETUP-negotiated versions in preference order; newer drafts negotiate via dedicated ALPNs (`version::ALPNS`). The version-to-behavior dispatch lives in `SetupVersion::from_version` (`setup.rs`).

## Invariants and footguns

- **No cascading abort**: Broadcast/Track/Group/Frame closes stay independent so handles can be shared. Closing or aborting one layer must not tear down its parent or siblings.
- **`moq_net::Timestamp` scales**: it's an instant, not a scalar, so it has no `+`/`-` operators. `checked_add`/`checked_sub` require matching scales and return `Err` (never panic) otherwise; `.convert()` to align scales first. `Ord::cmp` is scale-aware and safe, but `Eq`/`Hash` are structural (`from_secs(1) != from_millis(1000)`). `ZERO` is second-scale, so don't seed a `.max()` accumulator with it (a finer-scale value loses the tie-break); use an `Option` instead.

## Rust conventions

- **Use typed time units**: never represent a duration or timestamp as an untyped numeric value such as `f64` seconds or `u64` milliseconds. Use `std::time::Duration`, `moq_net::Timestamp`, or another type that carries the unit. When a serialized format requires a numeric value, convert at that boundary with `serde_with` where possible.
- **Retry loops use capped backoff with jitter** (root Retries has the policy). For a local loop, escalate a `Duration` toward a `const MAX`, jitter each wait (`delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0)`), and keep its budget next to the delay. Reuse an existing operation-specific retry abstraction when one owns the sequence already.
- **Prefer `kio` over tokio sync primitives**: reach for `kio::Producer`/`Consumer` (and the `poll_*` plumbing) instead of `tokio::sync` channels or `watch`. A `tokio::sync::watch` (or a channel) carrying a single value is a code smell. `kio` ties into the runtime-free `poll_*` model and avoids a hard runtime dependency.
- **Errors**: `thiserror` with `#[from]` for libraries, `anyhow` (with `.context("...")`, not `.map_err(|_| anyhow!())`) for binaries. Always `#[non_exhaustive]` on public error enums (e.g. `moq-net/src/error.rs`, `moq-ffi/src/error.rs`, `moq-loc/src/lib.rs`). Use `#[error(transparent)]` + `#[from]` for wrapped foreign errors (see `moq-token/src/error.rs`).
- **Config + TOML merge**: parse Usage defaults and environment first, recursively overlay the TOML value, then call `update_from` so only explicit CLI flags win. Precedence is `CLI > TOML > env > defaults`. This works without typing every field as `Option<T>`: `update_from` fills a declared default or an env value only where the standing value is still *empty*, and a plain value always reads as present. **Emptiness is a property of the type, and two shapes are not safe.** A bare `bool` reads empty when it is `false`, and a `Vec<T>` reads empty when it has no items, so either one is refilled from the environment (or a declared default) over whatever the file said. Type every merged boolean as `Option<bool>` and resolve the default in code (`stats::StatsConfig::enabled`, `web::WebConfig::resolved_ws`, `websocket::Config::resolved_enabled`). Bare `Vec<T>` fields carry the same latent bug and predate the Usage migration; see moq-dev/moq#3051 rather than materializing more of them. Everything else can be a bare field. See `moq-relay/src/config.rs` and its regression tests (`cli_does_not_clobber_toml_*`, `env_does_not_clobber_toml_booleans`); add such a test for any new flag.
- **Config structs**: derive `usage::Cli`, `Serialize`, and `Deserialize` with `#[serde(deny_unknown_fields, default)]`; declare flags with `#[usage(long, env = "MOQ_...")]`, flatten nested configs with `#[usage(flatten)]`, and use an `.init()`/`.load()` method to produce the live object. See the `#[non_exhaustive]` conventions below for whether the struct gets the attribute and/or a builder.
- **`#[non_exhaustive]`: do NOT add this by default.** Most public structs and enums should not have it; a diff that sprinkles it on new types is wrong. Its only job is to keep *adding* a field/variant from being a semver-breaking change, and it earns its keep in exactly three cases:

  1. Public error enums: always (see Errors above).
  2. A public enum that will realistically gain variants, so external `match`es keep compiling.
  3. A struct that will probably grow with additive, *defaultable* fields (the classic `Config`), paired with `Default`/a constructor so callers build via `default()`/`new()` + field set, not a struct literal. Prefer adding a field to such a struct over adding a positional parameter.

  Skip it everywhere else: on a struct that won't grow, or where a new field would *change behavior* rather than default to a no-op. There the addition should be a deliberate breaking change, not one the attribute waves through.
- **Enum variant order**: append new variants to the end of a public fieldless enum that uses implicit discriminants. Inserting one earlier changes the numeric values exposed by `as`, which is a semver break even when the enum is `#[non_exhaustive]`.
- **`libmoq` is a staticlib, so its C structs are cheap to extend.** The release tarball ships `include/moq.h` beside `lib/libmoq.a` and there is no cdylib, so a caller always compiles against the header matching the archive it links: appending a field is a recompile, not an ABI break. Configuration therefore belongs in a plain `#[repr(C)]` struct, not behind `create`/`set_*`/`get_*` functions. Two rules keep it additive: **append** new fields (never insert, so offsets don't move) and make **zero mean the previous behavior**, which needs a `has_*` / `*_valid` flag on any knob whose default is not zero (`websocket.enabled` defaults to true and the reconnect backoff to one second, so reading those straight out of a zeroed struct would silently disable them). `#[non_exhaustive]` does nothing here: it is rustc-only and cbindgen emits the whole struct regardless.
- **Builders** (private fields + chained `.with_x()` setters) are the orthogonal construction-ergonomics layer: reach for one when a struct has a lot of optional knobs, or is `#[non_exhaustive]` and you want construction to stay clean as fields get added (e.g. `select::Broadcast`).
- **Make misuse unrepresentable in the type system** (root Public API Scrutiny): make terminal operations consume `self` (e.g. `fn close(self)`) so use-after-close can't even be written, rather than `&mut self` plus a `closed` flag. Return owned handles whose `Drop` runs the cleanup instead of asking callers to remember a teardown call.
- **Borrow in, own out**: a parameter the callee only reads is a slice (`&[T]`, `&str`), and what you hand back is owned (`Vec<T>`, `String`). `fn publish(&mut self, encoded: &[Encoded])` accepts a `Vec`, an array, a boxed slice, or a sub-range without the caller rebuilding anything, and the signature already says the callee won't keep it. Take `Vec<T>` only when it genuinely takes ownership of the elements: it stores them (`I420::new(w, h, data: Vec<u8>)`) or moves fields out of them. When it merely consumes them once, `impl IntoIterator<Item = T>` says that without demanding a `Vec` the caller may not have.
- **Unwrapping**: prefer `if let Some(v) = x { ... }` / `let Some(v) = x else { ... };` over a `match` whose only job is to bind the inner value. Keep `match` when both arms do real work.
- **Naming / namespacing**: name by role, not by today's only implementation (`capture::Config`, `publish_capture`, not `CameraConfig`/`publish_camera`), so a second implementation slots in without a rename; don't bundle generic options under a specific-case name. Split a growing crate into role modules (`capture`, `encode`, `decode`) so each owns short, unprefixed names: the module supplies the prefix, so `encode::Config` beats `EncoderConfig` and `encode::Producer` beats `VideoProducer`. Don't nest a module whose name echoes its main type (`encode::encoder::Encoder` stutters): keep `mod encoder` private and re-export flat (`pub use encoder::{Encoder, Config}`) so it reads `encode::Encoder`.
- **Deprecation mechanics** (root Deprecation explains the why): a deprecated CLI flag stays a hidden alias (`alias_hidden = "..."`, since Usage advertises a plain `alias` in help and completions, or a separate `#[usage(..., hide = true)]` when it needs its own env var, its own runtime warning, or to be refused by name); a deprecated public item gets `#[doc(hidden)]` **and** `#[deprecated(note = "...")]`. A refused setting reports itself through `moq_tokio::Deprecated`, which each binary checks before it reads anything else; see `rs/moq-tokio/src/deprecated.rs`. Reach for the attribute: it fires at the *use* site, which is the whole point, while `#[doc(hidden)]` drops the symbol off docs.rs. What's banned is advertising the dead name on a published surface: no `--help` entry, and no "deprecated, use X" prose in the doc comment itself. Deprecating an item we still call internally also warns on our own call sites (CI runs `-D warnings`), so repoint those at the private helper.

## Binary setup

Binaries are `#[tokio::main] async fn main() -> anyhow::Result<()>`. Install the rustls crypto provider before anything TLS:

```rust
rustls::crypto::aws_lc_rs::default_provider().install_default().expect("crypto provider");
```

Then `Config::load()?` (initializes tracing), build clients/servers via `.init()`, and run an event loop with `tokio::select!`. See `moq-relay/src/main.rs`, `moq-bench/src/main.rs`.

## Testing

- `just check` lints and compiles the crates your branch changed plus every crate depending on them; `just test` runs their tests; `just fix` auto-fixes formatting/lint over the same set (`just rs _select` does the selection, via `cargo metadata`). `just check-all` / `just test all` / `just fix-all` cover every default member. `just rs test -p <crate>` (or `cargo nextest run -p <crate>`) for one crate.

- **`check` compiles default features for the selected workspace packages.** The workspace permutations live in `just rs features` (nightly): `--all-features` costs a full extra workspace compile that shares almost no artifacts with the default one (measured at ~6 minutes on top of an already-warm tree), and `--no-default-features` is a third distinct feature set that shares nothing with either. One crate is checked more tightly: when `moq-tokio` is selected, `check` also compiles that crate alone with `--no-default-features` and `--all-features`; the ordinary clippy pass is its default-feature build. That prevents a dependent's enabled transport from hiding a broken zero-feature configuration through workspace feature unification. The nightly workspace matrix remains the only thing that compiles every other crate's extremes, including moq-cli's `play`/`capture` and moq-audio's capture backend. `just rs audit` (cargo-deny) is nightly for the same workflow reason: an advisory is published without this repo changing.

- **`check` runs no `cargo check` pass.** Clippy is a superset of it, and the two use different rustc wrappers, so running both compiles the workspace twice for one set of errors.

- **Go through `just` rather than bare `cargo`.** Cargo fingerprints artifacts by emit kind and by compiler wrapper, so the same crate can sit in `target/` several times over. The expensive split is metadata versus codegen: `cargo check` and `clippy` emit metadata, while `cargo test` and `just test` codegen and link, and *dependencies* duplicate across that line. Within one emit kind it is cheaper than it sounds, since `just test` uses `RUSTC_WORKSPACE_WRAPPER`, which wraps only workspace crates, so dependencies stay shared with a plain `cargo test` and just our ~28 crates duplicate. It still adds up: each full tree is gigabytes, every agent worktree keeps its own, and ten worktrees were holding 59 GB on a 461 GB disk that had filled to 100%. If you run rust-analyzer, point it at clippy (`rust-analyzer.check.command = "clippy"`) so it shares with `just check` instead of opening another set.

- **Run tests through nextest, not `cargo test`.** `.config/nextest.toml` sets a
  `slow-timeout` with `terminate-after`, so a wedged test is reported as a
  TIMEOUT and killed; under `cargo test`'s harness the same test hangs forever,
  holding the target lock and burning a core. That matters here because a lost
  `kio` wakeup parks a task with nothing to wake it, which is a hang rather than
  a failure. `just rs test` uses nextest, and so does CI. Doctests are the
  one thing it skips (`just rs doctest` covers them), and `just rs loom` stays on
  `cargo test` since loom needs its own `--cfg loom` build.

- **A test flagged SLOW is a bug to fix, not a threshold to raise.** The whole
  workspace runs in well under a minute and the slowest single test is a few
  seconds, which is what makes the timeout above a meaningful signal. When a
  dependency is the reason (crypto and bignum code is orders of magnitude slower
  unoptimized), give it an `opt-level` override in the root `Cargo.toml` rather
  than shrinking what the test covers: `[profile.dev.package.<dep>]` applies to
  test builds too, and took the moq-token RSA keygen tests from 16s to 0.8s while
  still generating production-size 2048-bit keys.

- Rust tests are `#[cfg(test)] mod tests` inline in the source file.

- Async tests that depend on time call `tokio::time::pause()` first so timers fire instantly and deterministically (e.g. the tests in `moq-net/src/model/origin.rs`).

- Config-merge regressions belong next to the config (`moq-relay/src/config.rs::tests`); they serialize env mutation with a lock since Usage reads env.

- **Local checks only compile the host's platform and target, and PR CI is Linux-only.** `#[cfg(target_os = "...")]` code for other platforms is invisible to them, and `cargo fmt` skips those modules too. Windows and Mac runners cost too much for a per-PR gate, so those platforms are manual:

  - Windows (moq-video's Media Foundation and D3D11 backends): `just rs windows`, which must run ON Windows. You can't reproduce it elsewhere, since cross-compiling dies in openh264-sys2's vendored C++. It names `moq-cli/play` explicitly, since that feature is what pulls in moq-video's wgpu renderer and moq-audio's cpal output; a default-feature build compiles neither.
  - macOS (moq-video's VideoToolbox and ScreenCaptureKit, moq-audio's system audio): `just rs macos`, which must run ON macOS. Scoped to moq-video + moq-audio, and needs `--all-features` because moq-audio's capture backend is off by default.
  - Linux: covered nightly, not per-PR. `just rs features` runs `--all-features` in a dev shell carrying PipeWire and ALSA. VAAPI loads libva dynamically, so nvidia/vaapi/pipewire all compile without libva installed.
  - wasm32 (moq-wasm): `just rs wasm`. The crate root is `#![cfg(target_arch = "wasm32")]`, so a host-target `cargo check --workspace` compiles it down to nothing and sees no errors at all. This one needs no special host (the Nix shell carries the target), so `just check-all` always runs it and `just check` runs it whenever the diff touches any crate directory under `rs/`, not just `rs/moq-wasm/`: moq-wasm builds on moq-net, so a break in a dependency is invisible to every host-target pass. It's a compile gate, distinct from the root `just wasm`, which builds the shippable `@moq/wasm` package, and from `just test wasm`, which is the behavioral one: that runs the built bindings in headless Chromium against a real relay (`test/wasm/`, gated per-PR by `.github/workflows/wasm.yml`). Compiling says nothing about whether the bindings still open a session, which is how #2811 shipped on `main`.

  What still compiles these automatically, and when:

  - moq-video's platform backends are gated on `target_os` alone, and libmoq depends on moq-video, so a `libmoq-v*` tag builds them on `windows-latest` and Apple Silicon. That's a release-time backstop, not a PR one: a break lands on `main` and surfaces at the tag.
  - **moq-audio's macOS capture has no automated backstop at all.** ScreenCaptureKit system audio and the TCC pre-check sit behind the off-by-default `capture` feature, and every consumer leaves it off (libmoq and moq-ffi don't enable it; moq-cli's own `capture` feature is off in release builds). `just rs macos` is the only thing that compiles it, ever.
  - `.github/workflows/swift.yml` still runs on a Mac for `swift/**` and `rs/moq-ffi/**` PRs, so moq-ffi and the Swift wrapper keep a PR-time gate.

  Run the matching recipe by hand when you touch this code, and if you can't (no such host), say plainly in the PR that it's uncompiled rather than implying CI covered it.

- **`just rs loom` model-checks concurrent handoffs in kio and moq-net.** It stays outside `check` and `test`: `--cfg loom` swaps kio's Mutex/atomics for loom's instrumented ones, which rebuilds the whole dependency tree and can't share artifacts with a normal `cargo test`. Use it when developing or diagnosing concurrent handoffs. Budget about a minute of model checking on top of that build. The search is exhaustive on purpose, so don't reach for `preemption_bound` to speed it up; the recipe already buys the speed back with `--release`, which matters here because a model check reruns the body once per interleaving.

  Loom permutes every thread interleaving instead of hoping a stress loop hits the bad one. It caught a `ProducerWeak::produce` race that had been live for months, on iteration 4. Reading the results:

  - A **hang is a finding, not a flake**: a parked `loom::future::block_on` that never wakes leaves every thread blocked, which loom reports as a deadlock. That's how a lost wakeup surfaces.
  - **"Arc leaked"** means a reference cycle, usually a handle stored inside the state it points at. That's what `kio::Weak` (as opposed to `ProducerWeak`, which keeps the allocation) exists to avoid.
  - Before trusting a *passing* model, mutate the code it covers and confirm it fails. A model that never exercises the race is worse than none.

  Two constraints when adding to it: every non-loom `#[cfg(test)]` in kio must be `#[cfg(all(test, not(loom)))]`, or the tokio tests build loom primitives outside `loom::model` and panic; and loom's `Arc` has no `downgrade`, so `lock.rs` and `waiter.rs` keep std's (see `kio/src/sync.rs`).
