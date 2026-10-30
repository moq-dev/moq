# [L] Complete the browser WASM bindings

## Goal

The Rust browser bindings support publishing and consumption for lite and
moq-transport, including the model behavior required by `@moq/net` consumers.
The chosen binding becomes a tested replacement candidate;
this quest does not switch the public package's default implementation.

## Plan

Decided 2026-10-12: reuse the existing Rust implementation, including async.
Inventory `@moq/net` and its in-tree consumers against the bindings first.
Preserve frame metadata as well as bytes, exact sequence/id values, ownership,
close/error behavior, cancellation, and backpressure. Validate supported
versions and transports with the browser fixtures and `just test interop --all`.
Wire new tests into CI, at least nightly.

Decided with the maintainer on 2026-10-17 (#5838, report in
[`test/wasm/generator.md`](/test/wasm/generator.md)): complete `moq-ffi` through UniFFI
over ubrn's `wasm2` player, not the hand-written `moq-wasm` and not BoltFFI. This quest
completes moq-ffi's browser parity and generates its TypeScript in CI, extending
`test/wasm/uniffi/` (`just test wasm-baseline --uniffi`, nightly). `moq-wasm`'s deletion
moves to [adoption](/quest/m2/rs2ts/wasm-adopt.md). Until
[ubrn reads UniFFI 0.32](/quest/m2/rs2ts/ubrn-032.md), generation keeps applying
`test/wasm/uniffi/pin.patch` to a scratch checkout.

Gaps the spike found against `@moq/net` and moq-wasm: moq-ffi has no WebSocket
fallback on wasm, and payload copies cost about 7 to 8 ns
per byte (see [Payload bytes](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-bytes.md)). Measure boundary calls, Promise allocation,
payload copies, and retained memory. Compare a batched poll/event-drain
boundary if these dominate, including latency and backpressure effects;
do not require a sans-IO rewrite or impose batching latency to win throughput.
Keep compiler-specific source refactors out until a selected backend needs
them. Benchmark any hot-path changes, including publisher/subscriber sweeps.

Rust and JS must agree on clock origin, units, deadlines, and controlled-time
behavior across the boundary. Integrate with moq-clock/@moq/clock if available;
otherwise preserve current semantics without blocking on the whole time line.

Extend the [backend baseline](/test/wasm/README.md#backend-baseline) for full API parity.

Update the bindings' README and existing API docs for the surface added.

Public API: extends the unpublished browser bindings; any changes needed to
`@moq/net` are proposed in the adoption quest. Wire: none.
