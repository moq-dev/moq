# [M] Session benches sweep transport and version

## Goal

The moq-net session benches run each scenario over transport x version, not
only the in-memory mock, so a cost that one transport adds (stream opens,
framing, flow control) shows up as its own series in the regression table.

## Plan

Decided 2026-10-21 with the maintainer: m2. `rs/moq-net/benches/session.rs`
sweeps `VERSIONS` (lite-06, lite-07-wip, moq-transport-22) over the
in-memory transport only. Add the transports a production session uses,
moq-quic and qmux at least, keeping each run deterministic: drive QUIC over
an in-memory datagram link rather than real sockets if the noise demands it.
Keep existing bench IDs stable so their history carries over; new
transports add series rather than renaming old ones. This bench owns the
qmux series in the regression table; the stream transport micro-bench
(`rs/qmux/examples/bench.rs`) stays a local profiling tool (decided
2026-10-22, maintainer).

Public API: none. Wire: none.

## Related

- [Benchmark sources](/quest/m2/bench-sources.md) - feeds these series into the regression table
- [QUIC on the sim](/quest/m2/quic-sim.md) - an in-memory QUIC link the sweep could reuse
