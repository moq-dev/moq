# [M] qlog traces from io_uring workers

## Goal

`--server-quic-qlog <DIR>` writes one trace file per connection in io_uring
mode, the same as it does on the tokio workers. Today `uring::transport()`
refuses the setting outright ("io_uring workers cannot write qlog traces"), so
a debugging or congestion-control run has to fall back to a different runtime
and therefore a different data path than the one being debugged.

Gated by the existing `qlog` feature, so a production build still compiles
none of the machinery.

## Plan

Both backends, not just the default: quiche compiles qlog support in
unconditionally and quinn-proto has its own `qlog` feature, so
`--server-quic-qlog` must mean the same thing whether the relay was built with
`io-uring` or `io-uring-quinn`. Drop the bail in `transport()` once both are
wired.

The open piece is where the bytes land. A pinned worker must not block on a
file, so write through the ring if the adaptation is clean: the QUIC stacks
want a `std::io::Write`, and a shim that stages into pool-owned buffers and
submits `IORING_OP_WRITE` is the same ownership shape the sender already uses
for its TX pool. If that turns out to fight the `Write` contract (partial
writes, flush semantics, completion backpressure), fall back to handing the
buffered bytes to the shared tokio runtime's blocking pool rather than growing
a bespoke writer thread. Either way, document which one shipped and why.

Traces are per connection and named from the connection id, matching what the
tokio path already produces, so an existing qlog workflow reads both without
knowing which runtime wrote them.

## Related

- [Worker metrics](/quest/m1/uring-metrics.md) - the other half of io_uring-mode
  observability, on the counter side rather than the trace side
