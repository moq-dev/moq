# [M] Choose the browser binding generator

## Goal

A reproducible report compares three Rust browser bindings on the baseline
harness: TypeScript that UniFFI generates from `rs/moq-ffi`, TypeScript
that BoltFFI generates from it, and the hand-written wasm-bindgen
`rs/moq-wasm`. The maintainer picks which one `@moq/net` wraps, and the
report records that decision. This quest does not adopt any binding,
delete `moq-wasm`, or evaluate JS-only output.

## Plan

Decided with the maintainer on 2026-10-13:

- Spike before [Complete WASM bindings](/quest/m2/rs2ts/wasm-bindings.md)
  grows `moq-wasm`. Every native language already binds through `moq-ffi`.
  [#2907](https://github.com/moq-dev/moq/issues/2907) argued the browser
  should too, rather than keep a second hand-written surface.
- Bind `moq-ffi` as it is, not a browser-only UniFFI crate:
  [#5460](https://github.com/moq-dev/moq/pull/5460) rejected per-layer crate
  splits. The generated TS tree-shakes, but the `.wasm` does not. The linker
  keeps every UniFFI export, so only cargo features reduce size. Toggling
  `default-features` changes nothing on wasm32 (`audio` and `video` are
  already native-only), so report each module's share of the `.wasm`
  instead. If size decides the result, propose a net-only feature rather
  than a new crate.
- Use [uniffi-bindgen-react-native](https://github.com/jhugman/uniffi-bindgen-react-native)
  (ubrn), whose `wasm2` flavour is a plain wasm32 cargo build driven by a
  generic JS player. Upstream UniFFI will not ship a JS generator and points
  to ubrn. ubrn pins UniFFI 0.31, while `moq-ffi` is on 0.32. The spike
  branch therefore pins `moq-ffi` to 0.31 for measurement only and does not
  land that pin. `wasm2` also needs the matching `uniffi-runtime-wasm`
  dependency and an `extern crate uniffi_runtime_wasm as _` to force its
  link (the player looks for `__ubrn_alloc`). Add both on the spike branch
  and count the runtime in the shipped size. If UniFFI wins, add a waiting quest for ubrn 0.32 support
  ([#468](https://github.com/jhugman/uniffi-bindgen-react-native/pull/468)),
  required by adoption but not by this report.
- [BoltFFI](https://github.com/boltffi/boltffi) is the third candidate
  (decided in [#5498](https://github.com/moq-dev/moq/pull/5498), whose
  BoltFFI spike covers the native half). Neither quest blocks the other.
  BoltFFI has its own macros, so it cannot bind `moq-ffi` as-is. Reuse
  that spike's slice crate, [`bench/ffi-gen`](/bench/ffi-gen/README.md),
  and measure UniFFI on the same slice so the per-call numbers compare
  like for like. It carries BoltFFI's markers literally, with a shim crate
  mapping them to UniFFI's, because BoltFFI's scanner does not see
  `cfg_attr`. Its `run.sh build` already produces both `.wasm` files and
  BoltFFI's web package; ubrn has no 0.32 support, so no UniFFI JS. Full `moq-ffi` through UniFFI still provides the size and
  coverage numbers. The slice is a feasibility filter for BoltFFI, not
  selection evidence: if BoltFFI passes and is favored, port the harness
  workload to it and rerun the parity, size, startup, and memory cases
  before the maintainer picks it. BoltFFI ships its own wasm loader and
  `.d.ts` files for web, node, and bundlers. Async uses poll plus a
  continuation, like UniFFI. Crates using `web-sys` trapped at runtime
  until [boltffi#943](https://github.com/boltffi/boltffi/pull/943)
  (2026-09-28, released in v0.31.0). Async constructors bind in v0.31.1,
  though [boltffi#95](https://github.com/boltffi/boltffi/issues/95) is
  still open; TS types the slice's `MoqSession.dial` as
  `Promise<MoqSession | null>`. TypeScript streams are experimental
  ([boltffi#40](https://github.com/boltffi/boltffi/issues/40)). Roughly
  one maintainer.

What already exists: `moq-ffi` compiles for wasm32
([#2911](https://github.com/moq-dev/moq/pull/2911)) with
`wasm-unstable-single-threaded`, including the media layer. Native codecs,
the server, and the TLS knobs are gated out. The #2907 spike generated TS
(with a different generator) that passes `tsc --strict`. It maps u64 to
bigint and lets a handle be freed while one of its calls is pending.
None of it has run in a browser.

Measure on the [baseline harness](/test/wasm/README.md#backend-baseline):

- Shipped size (compressed `.wasm` plus JS player and glue), cold start,
  first frame, per-frame CPU, and memory.
- Per-async-call cost, since UniFFI resolves one JS Promise per
  `rust_future_poll` wakeup. Compare it with wasm-bindgen's async glue.
  Sweep publisher and subscriber counts.
- Parity on the harness cases: exact u64 above 2^53, cancellation through
  `AbortSignal`, close/error propagation, and freeing a handle mid-call.
- `ffi::Task::run` awaits in place on every target: a cancelled foreign
  call stops polling, and its future drops at `rust_future_free`. Test that
  aborting or freeing a pending call releases its state without stealing
  the next call's event.
- Things the binding cannot do. `JsValue` cannot cross the boundary, so
  Rust constructs the WebTransport, as `moq-ffi` already does. Cover the
  supported fallback transport as well.
- Deployment: ubrn's player compiles dispatchers with `new Function` and
  falls back to an interpreter under a strict CSP. Measure both modes. Note
  open ubrn wasm issues such as TS that fails to type-check
  ([#472](https://github.com/jhugman/uniffi-bindgen-react-native/issues/472)).
- The JS-only path: whether the player can drive wasm2js output, as an
  input to the [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md).

There are no invented thresholds; the maintainer weighs the report. Update
the wasm-bindings quest for the chosen binding. If a generated binding
wins, that quest completes `moq-ffi`'s browser parity and generates the TS in CI, and
`moq-wasm`'s deletion moves to [adoption](/quest/m2/rs2ts/wasm-adopt.md).

Public API: none. Wire: none.

## Related

- [Binding namespaces](/quest/m1/ffi-shape/README.md) - the `moq-ffi` shape a generated browser binding inherits
