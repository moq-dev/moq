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

## Status: compiles and ships a typed JS package; one runtime blocker left

What works today:

- **The architecture is right.** `moq-net` is generic over
  `web_transport_trait::Session` and spawns via `web_async::spawn` (not
  `tokio::spawn`), so it is not tied to native QUIC.
- **The WebTransport adapter is complete** (`src/transport.rs`): a newtype
  bridge from `web-transport-wasm` (browser WebTransport) to the
  `web-transport-trait` abstraction `moq-net` consumes. The orphan rule forces
  the newtypes; the shapes line up almost 1:1.
- **It compiles to `wasm32-unknown-unknown` and produces `@moq/wasm`**: `just
  wasm` emits a typed, importable package (`Session` / `Broadcast` / `Track` /
  `Group`, used as `Moq.Session` etc. via `import * as Moq`, `Promise`-returning
  methods, `.d.ts`). Verified: it bundles under esbuild and a strict-mode TS
  consumer type-checks against it.
- Scope is the consume path (connect -> broadcast -> track -> group -> frame),
  the `@moq/watch` use case. The publish path follows the same shape.

### Three moq-net changes this required (all landed here)

1. tokio's `test-util` feature moved from moq-net's main deps to dev-deps
   (it is test-only and unsupported on wasm).
2. `Send`/`Sync` assumptions relaxed to `MaybeSend`/`MaybeSync`: the browser
   transport is `!Send`, but `SessionInner` and a couple of `.boxed()` sites
   hard-coded `Send`. A cfg-gated `MaybeSendBox` / `.maybe_boxed()` helper
   (`rs/moq-net/src/util.rs`) picks `BoxFuture`+`boxed()` on native and
   `LocalBoxFuture`+`boxed_local()` on wasm. Native behavior is unchanged
   (`MaybeSend`/`MaybeSync` *are* `Send`/`Sync` there).
3. Timers and `Instant` routed through `web_async::time` instead of
   `tokio::time` (session poll interval, subscriber linger, probe interval,
   track-cache eviction). `web-async` 0.1.4 re-exports `tokio::time` on native
   and `wasmtimer` (a `performance.now()` + `setTimeout` shim) on wasm, so the
   same code runs on both. tokio's clock is `std::time::Instant::now()`, which
   *panics* on wasm (no clock) under `spawn_local` (no time driver); wasmtimer
   fixes that. Native unchanged: `web_async::time::Instant` *is*
   `tokio::time::Instant` there, so `tokio::time::pause`/`advance` test clocks
   still work (367 tests pass).

These touch the wire layer, so the PR should target `dev`.

### Not ported: the wall-clock anchor (and it doesn't need to be)

`model/time.rs`'s `TIME_ANCHOR` uses `std::time::Instant::now()` +
`SystemTime::now()` (both panic on wasm) to map a monotonic instant to a
jittered wall-clock `Timestamp`. It looks like a wasm hazard, but it's a
`LazyLock` reached only through `Timestamp::now()` / `From<Instant>`, and
**nothing in the repo calls those** (frames carry wire timestamps; cache
eviction uses monotonic `Instant`). So the anchor never initializes and never
panics on wasm. It's left as an unused public helper.

If a caller ever materializes (e.g. a publish helper that stamps capture time
locally), porting it would mean a portable `SystemTime` (wasmtimer already has
`wasmtimer::std::SystemTime`), but that's not needed today.

### Out of scope here: moq-mux

Media muxing (`moq-mux`) is not yet wasm-ready: `hang` and `moq-mux` enable
tokio's `fs` feature (native filesystem), unsupported on wasm32. Feature-gating
`fs` behind a native-only cfg in those crates is a prerequisite. The `moq-mux`
dependency is commented out in `Cargo.toml` until then.

## Building

`just wasm` (from the repo root) does everything: builds for wasm, runs
`wasm-bindgen` (web target) into `js/wasm/dist`, and shrinks with `wasm-opt`.
The wasm target, the cfg flags (`getrandom` wasm backend + web-sys unstable
WebTransport APIs), and the `wasm-bindgen-cli` / `binaryen` tools come from
`.cargo/config.toml` and the Nix dev shell.

To build the crate alone:

```bash
cargo build -p moq-wasm --target wasm32-unknown-unknown --release
```
