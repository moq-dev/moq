# [XS] io_uring demux does per-connection work once per completion

## Goal

`Endpoint::demux` (`rs/moq-uring/src/quic/driver/endpoint.rs`) updates
each fed connection's wire counters once per receive completion instead of
once per datagram, and a completion that feeds one connection allocates no
`fed` list.

## Plan

From the 2026-10-27 audit. Per datagram routed to a connection, demux calls
`conn.conn.borrow().stats()`, which copies the whole `ConnectionStats` and
queries the congestion controller's metrics only to read `udp_rx.bytes`.
Per completion, it allocates `fed: Vec<ConnectionHandle>` on the first push,
though a GRO completion almost always feeds exactly one connection.

Set `received_bytes` once per fed connection in the kick loop after the
batch. Keep the first fed connection inline, and fall back to a list
only when a completion mixes connection IDs. Every connection a mixed batch
feeds must still be kicked.

Measure with `just bench-runtime` at both revisions on the same host and
worker count, plus allocations per completion, with GRO on and off and one
versus several connections per completion. The payload copy out of the receive pool is
[QUIC I/O ownership](/quest/m2/quic-io-boundary.md)'s, not this quest's.

Public API: none. Wire: none.
