# [M] Prove receive memory with JavaScript senders and benchmarks

## Goal

A JavaScript sender drives both native receivers into receive-credit
exhaustion and they recover, and nightly measures what bounded receive memory
costs. This is coverage for the message-assembly work in #5735, with no new
transport behavior.

## Plan

Split out of #5735, which lands native credit holding, pressure recovery, and
the minimum receive window with Rust-only end-to-end tests over tokio and
io_uring.

A JavaScript sender over the stock receivers cannot stall them. The minimum
window fits the largest unit, and a strict-priority sender finishes one
stream before the next, so every frame completes. The test therefore needs a
receiver whose connection window is far below the minimum. Neither stock
config allows that, so the test assembles a WebTransport (HTTP/3) server
from each raw driver: tokio's `quic::driver` with `quic::web::Server`, and
an io_uring endpoint in web mode. Keep it test-only; do not add a public way
to configure a window below `MIN_RECEIVE_WINDOW`.

The sender is a Bun script using `@moq/web-transport`, pinned to the test
server's certificate hash with WebSocket disabled. It mirrors the Rust tests
in `rs/moq-tokio/tests/integration/receive_credit.rs`. A low-priority track sends a frame
larger than the window. A high-priority frame then has to complete, and a
later low-priority group has to arrive after the low stream is stopped. Run
it from Rust the way `rs/moq-tokio/tests/integration/max_age.rs` drives its Bun client,
and wire it into the interop workflow, since normal CI has no Bun.

The benchmark sweeps concurrent streams and frame sizes, and compares a
finite stream window with one that follows the connection window. Report
retained bytes (held plus unread credit), throughput, and frame completion
latency. Sweep streams and frame sizes on separate axes, so a cost that grows
with the stream count shows up as a slope. Wire it into `sh/gh/nightly.sh`
next to `benchmark_held_reads`. Unit tests stay on mocked time; the benchmark
is the only wall-clock part.
