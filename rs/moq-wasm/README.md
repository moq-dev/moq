# moq-wasm (experiment)

Compile the real `moq-net` Rust implementation to WebAssembly and expose it to
JavaScript via `wasm-bindgen`, driving the browser's native WebTransport from
inside WASM. The goal: replace the hand-written TypeScript moq-lite/moq-ietf
wire implementation in `@moq/net` (~10k LOC) with the canonical Rust one, so the
protocol lives in exactly one place.

This crate is the Rust half; the generated JS package is
[`@moq/wasm`](../../js/wasm) (`just js wasm` builds it). It is **not** the same as
`moq-ffi`: that crate uses UniFFI, which targets the C ABI (Kotlin/Swift/Python/
Go). Browsers need `wasm-bindgen`, so this is a separate sibling crate. (For
*React Native* JS, `uniffi-bindgen-react-native` can reuse `moq-ffi` directly;
that path is unrelated to this crate.)

## Status: compiles and ships a typed JS package

What works today:

- **Executor-independent sessions.** `moq-net` is generic over
  `web_transport_trait::poll::Session` and returns a driver for the caller to
  run. `moq-wasm` spawns `moq_net::time::run` via `web_async::spawn`, which
  supplies the browser clock and sleeps until the driver's next deadline.
- **The browser transport needs no adapter**: `web-transport-wasm` implements
  the poll traits `moq-net` consumes, so `src/transport.rs` is just the dial
  (the ALPN list and the browser's two trust modes).
- **It compiles to `wasm32-unknown-unknown` and produces `@moq/wasm`**: `just
  wasm` emits a typed, importable package (`Session` / `Broadcast` / `Track` /
  `Group`, used as `Moq.Session` etc. via `import * as Moq`, `Promise`-returning
  methods, `.d.ts`).
- Scope is the consume path (connect -> broadcast -> track -> group -> frame),
  the `@moq/watch` use case. The publish path follows the same shape.
- **It is tested in a browser**: `just test wasm` runs the built package in
  headless Chromium against a real relay, one per protocol flavour, publishing
  with `@moq/net` and subscribing with these bindings
  ([`test/wasm/`](../../test/wasm)). `just rs wasm` only compiles the crate, so
  that harness is the only thing that can tell a working binding from one that
  merely builds.

### Three moq-net changes this requires

1. tokio's `test-util` feature moved from moq-net's main deps to dev-deps
   (it is test-only and unsupported on wasm).
2. `Send`/`Sync` assumptions relaxed to `MaybeSend`/`MaybeSync`: the browser
   transport is `!Send`, but `SessionInner` used to hard-code `Send`.
   `web_async::MaybeSendBoxFuture` picks a `Send` boxed future on native and a
   local boxed future on wasm. Native behavior is unchanged.
3. Drivers accept `moq_net::time::Instant` and return their next deadline.
   `moq_net::time::run` supplies the clock and one re-armable sleep through
   `web_async::time`, backed by `performance.now()` and `setTimeout` here.
   `moq-net` handles session sampling, linger, probes, and cache expiration
   using the supplied time and never spawns tasks.

### Timestamp fallback

`moq_net::time::Instant` is `std::time::Instant` on native and the
wasmtimer-backed instant from `web_async::time` in the browser, where
`std::time::Instant` panics. `model/time.rs` anchors its timestamps on it.

### Out of scope here: moq-mux

Media muxing (`moq-mux`) is not yet wasm-ready: `hang` and `moq-mux` enable
tokio's `fs` feature (native filesystem), unsupported on wasm32. Feature-gating
`fs` behind a native-only cfg in those crates is a prerequisite. The `moq-mux`
dependency is commented out in `Cargo.toml` until then.

## Building

`just js wasm` (from the repo root) does everything: builds for wasm and runs
`wasm-bindgen` (web target) into `js/wasm/dist`. The wasm target, the cfg flags
(`getrandom` wasm backend + web-sys unstable WebTransport APIs), and the
`wasm-bindgen-cli` tool come from `.cargo/config.toml` and the Nix dev shell.

To build the crate alone:

```bash
cargo build -p moq-wasm --target wasm32-unknown-unknown --profile wasm-release
```
