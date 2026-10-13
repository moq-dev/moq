# [L] Complete the browser WASM bindings

## Goal

The Rust browser bindings support publishing and consumption for lite and
moq-transport, including the model behavior required by `@moq/net` consumers.
The existing consume-only experiment becomes a tested replacement candidate;
this quest does not switch the public package's default implementation.

## Plan

Decided 2026-10-12: reuse the existing Rust implementation, including async.
Inventory `@moq/net` and its in-tree consumers against the bindings first.
Preserve frame metadata as well as bytes, exact sequence/id values, ownership,
close/error behavior, cancellation, and backpressure. Validate supported
versions and transports with the browser fixtures and `just test interop --all`.
Wire new tests into CI, at least nightly.

Start with the existing bindings. Measure boundary calls, Promise allocation,
payload copies, and retained memory. Compare a batched poll/event-drain
boundary if these dominate, including latency and backpressure effects;
do not require a sans-IO rewrite or impose batching latency to win throughput.
Keep compiler-specific source refactors out until a selected backend needs
them. Benchmark any hot-path changes, including publisher/subscriber sweeps.

Rust and JS must agree on clock origin, units, deadlines, and controlled-time
behavior across the boundary. Integrate with moq-time/@moq/time if available;
otherwise preserve current semantics without blocking on the whole time line.

Update the bindings' README and existing API docs for the surface added.

Public API: extends the unpublished browser bindings; any changes needed to
`@moq/net` are proposed in the adoption quest. Wire: none.

## Required

- [Browser backend baseline](/quest/m1/rs2ts/spike.md) - the measurements and parity harness this extends
