# [M] QUIC connection lock and packet dispatch profile

## Goal

A measured verdict on two costs moq-tokio's QUIC driver inherited from quinn,
each lever built only if the profile shows its cost: time lost to the
connection-wide mutex that every stream operation and the connection task
share, and the per-packet hop from the endpoint task to the connection task.

## Plan

Planned 2026-10-23 (maintainer) from the moq-quic API audit. Measure on the
shape the [moq-quic API](/quest/m1/quic-api/README.md) epic leaves behind,
since the async-to-poll adapter and leases that hold the lock both go first.

- **Lock contention.** One `Mutex<State>` per connection guards moq-quic's
  state; the driver takes it at about 58 sites, from every stream handle on
  any worker thread, and from the connection task for receive, transmit,
  and timers. In fanout, each group's task writes its own stream from a
  different worker. Measure lock wait time and cache-line traffic
  (`perf c2c`) with `just bench-profile`, sweeping connections × streams per
  connection × tokio workers, so a cost that grows with the table shows as a
  slope. Compare CPU per Gbps with moq-uring, which keeps a connection on
  one core and has no such lock.
- **Packet dispatch.** The endpoint task demuxes each received datagram into
  an unbounded channel to its connection's task, with a wake per packet
  (`driver/endpoint.rs`, the receive loop), and endpoint events flow back the
  same way. Count channel sends, wakes, and cross-thread hand-offs per packet.
  The channel is unbounded, and stream credit does not bound encrypted
  datagrams waiting before the connection task processes them. Measure its
  depth and bytes when a peer's ingress outruns its connection task (lock
  wait included). If it grows, bound it by design at the packet boundary
  (batch dispatch, or backpressure into the socket read) rather than with a
  queue cap that drops (2026-10-27 audit, maintainer).

Levers, only where the profile shows a cost:

- A per-stream staging queue: a write hands its owned `Bytes` (the `Buf`
  API from [moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)) to the stream, bounded
  by the stream's flow-control window so backpressure is unchanged, and the
  connection task moves it into moq-quic under its own lock.
- Dispatch a whole GRO batch per connection in one message, coordinated
  with [the I/O boundary redesign](/quest/m2/quic-io-boundary.md), whose
  "one `handle` call takes a whole GRO batch" is the state-machine half.
- Stream handles that carry a slab key instead of a `StreamId`, removing
  moq-quic's per-stream lookups (the driver's own maps go with
  [waits](/quest/m1/quic-api/waits.md)). FxHash is cheap, so this is last
  and only on evidence.

The maintainer wants the connection state free of runtime borrow checks
altogether (2026-10-25); [borrow-free state](/quest/m2/perf/quic-borrow-free.md)
designs that, and this profile is its baseline.

A measured no-win is a valid outcome. Report public API and wire impact; the
staging queue and batch dispatch should need none.

## Required

- [Native WebTransport](/quest/m1/quic-api/native-web.md) - removes the adapter's per-operation boxing, which would otherwise dominate the profile
- [Waits](/quest/m1/quic-api/waits.md) - moves the waiter tables under the lock this profiles

## Related

- [Borrow-free state](/quest/m2/perf/quic-borrow-free.md) - the lock-free designs this profile is the baseline for
- [Relay egress profile](/quest/m2/quic-egress-profile.md) - the transmit side of the same path
- [Relay allocations](/quest/m2/perf/relay-allocs.md) - the allocation profile of the same workload
