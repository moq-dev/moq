# moq-wasm (experiment)

Compile the real `moq-net` Rust implementation to WebAssembly and expose it to
JavaScript via `wasm-bindgen`, driving the browser's native WebTransport from
inside WASM. The goal: replace the hand-written TypeScript moq-lite/moq-ietf
wire implementation in `@moq/net` (~10k LOC) with the canonical Rust one, so the
protocol lives in exactly one place.

This crate is the Rust half; the generated JS package is
[`@moq/wasm`](../../js/wasm) (`just wasm` builds it). It is **not** the same as
`moq-ffi`: that crate uses UniFFI, which targets the C ABI (Kotlin/Swift/Python/
Go). Browsers need `wasm-bindgen`, so this is a separate sibling crate. (For
*React Native* JS, `uniffi-bindgen-react-native` can reuse `moq-ffi` directly;
that path is unrelated to this crate.)

## Surface

The modules mirror `moq-net`'s role modules one-for-one, and each type binds the
methods of its counterpart:

| module | JS classes | mirrors |
|---|---|---|
| `session` | `Session` | `moq_net::Session` + `origin::{Producer, Consumer}` |
| `broadcast` | `BroadcastProducer`, `BroadcastConsumer` | `moq_net::broadcast` |
| `track` | `TrackProducer`, `TrackConsumer`, `TrackSubscriber`, `TrackRequest` | `moq_net::track` |
| `group` | `GroupProducer`, `GroupConsumer` | `moq_net::group` |
| `announce` | `AnnounceConsumer`, `Announce` | `moq_net::announce` |
| `options` | `Subscription`, `TrackInfo`, `Frame` | the plain-data types |

That mirroring is the point: the binding is the surface most likely to drift
from the crate it wraps, so the two are meant to be read side by side. When
`moq-net` grows an API a browser caller needs, add it to the matching module
here rather than starting a new flat entry point. See the note in `rs/CLAUDE.md`
about `moq-wasm` being a binding that nothing but a compile gate covers.

Types carry the role as a prefix (`TrackProducer`, not `track::Producer`),
against the usual convention of letting the module supply it. wasm-bindgen
resolves a type in a signature by its Rust ident alone, ignoring both the module
path and `js_name`, so two modules each exporting a `Consumer` silently generate
typings where one stands in for the other. The modules stay private and
re-export flat, so nothing reads `broadcast::BroadcastProducer`.

### Conventions at the boundary

- Durations are milliseconds and timestamps are microseconds, matching `@moq/net`.
- Sequence numbers stay `u64`, which wasm-bindgen maps to a JS `bigint`.
- Closing takes an application close code (`moq_net::Error::App`); a JS `Error`
  has nothing to map onto the wire.
- `closed()` rejects rather than resolving: every close carries a reason.
- Async methods take `&self` and must produce `'static` futures, so a handle
  with an in-flight call moves its value out for the duration (`util::Exclusive`).
  A re-entrant call errors instead of aliasing: one at a time per handle.

### Not covered

Relay-side concerns are deliberately absent: routes, hops, cost, origin scoping,
and cluster identity have no browser caller. So are the `poll_*` variants, since
JS has no equivalent of a `kio::Waiter`.

Media muxing is still out (see below), and there is no WebSocket fallback: the
Rust `qmux` crate is tokio-based, so a browser session is WebTransport-only
where `@moq/net` can fall back.

### Three moq-net changes this requires

1. tokio's `test-util` feature moved from moq-net's main deps to dev-deps
   (it is test-only and unsupported on wasm).
2. `Send`/`Sync` assumptions relaxed to `MaybeSend`/`MaybeSync`: the browser
   transport is `!Send`, but `SessionInner` used to hard-code `Send`.
   `web_async::MaybeSendBoxFuture` picks a `Send` boxed future on native and a
   local boxed future on wasm. Native behavior is unchanged.
3. Timers and `Instant` routed through `web_async::time` instead of
   `tokio::time` (session poll interval, subscriber linger, probe interval,
   track-cache eviction). `web-async` re-exports `tokio::time` on native
   and `wasmtimer` (a `performance.now()` + `setTimeout` shim) on wasm, so the
   same code runs on both. tokio's clock is `std::time::Instant::now()`, which
   *panics* on wasm (no clock) under `spawn_local` (no time driver); wasmtimer
   fixes that. Native unchanged: `web_async::time::Instant` *is*
   `tokio::time::Instant` there, so `tokio::time::pause`/`advance` test clocks
   still work.

### Timestamp fallback

`model/time.rs` uses `web_async::time::{Instant, SystemTime}` for timestamp
generation. Native keeps the Tokio-backed instant so paused-time tests still
work; browser wasm uses wasmtimer-backed clocks, avoiding the `std::time` and
Tokio paths that panic or lack a driver on `wasm32-unknown-unknown`.

### Out of scope here: moq-mux

Media muxing (`moq-mux`) is not yet wasm-ready: `hang` and `moq-mux` enable
tokio's `fs` feature (native filesystem), unsupported on wasm32. Feature-gating
`fs` behind a native-only cfg in those crates is a prerequisite. The `moq-mux`
dependency is commented out in `Cargo.toml` until then.

## Building

`just wasm` (from the repo root) does everything: builds for wasm and runs
`wasm-bindgen` (web target) into `js/wasm/dist`. The wasm target, the cfg flags
(`getrandom` wasm backend + web-sys unstable WebTransport APIs), and the
`wasm-bindgen-cli` tool come from `.cargo/config.toml` and the Nix dev shell.

To build the crate alone:

```bash
cargo build -p moq-wasm --target wasm32-unknown-unknown --profile wasm-release
```
