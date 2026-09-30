# [M] Relay egress syscall and allocation profile

## Goal

A measured verdict on the relay's egress costs below moq-net: syscalls per
train, allocations and copies per frame, and whether a released GSO train
bursts at the bottleneck. Each lever below is built only if the profile
shows its cost; a lever measured within noise is recorded and closed.

## Plan

Decided in the 2026-09-30 audit: send batching, kernel pacing, and send
buffer pools merged into this one measure-first quest, since all three are
questions about the same egress path and share one profile.

Measure first, on the chat and fanout shapes with many connections, tokio
workers versus io_uring workers, with the existing profiling captures:

- Syscalls: the tokio path sends one `sendmsg` per GSO train per connection;
  the io_uring path queues one SQE per train and submits them together. Count
  syscalls per second and CPU per Gbps, and confirm from the io_uring
  worker's `enters` counter that the ring already amortizes.
- Allocations: noq's `SendBuffer` keeps a write above 1452 bytes as the
  caller's `Bytes` and coalesces smaller writes into a `BytesMut`. Count
  allocations and bytes copied per frame, split by frame size, so the share
  of sub-threshold copies is known.
- Bursts: noq paces inside `poll_transmit` on both runtimes (#4400,
  rs/moq-uring/src/quic/noq/connection.rs:814-817), so the only open question
  is whether a released GSO train leaves the NIC as a burst the bottleneck
  cannot absorb. On a netem bottleneck, compare inter-packet gaps and queue
  occupancy for a 64-segment train against the same bytes paced at the
  controller's rate.

Then, only where the profile shows a cost:

- `sendmmsg` in `moq-noq-udp` behind the existing GSO path, submitting every
  train ready across connections in one call, falling back to per-train
  `sendmsg` on partial failure the way the GSO fallback does.
- A `BufFactory`-style seam on `SendBuffer` in the fork that takes buffers
  from a pool sized from the send window and returns them on ACK.
- `SCM_TXTIME` stamps, one per train, at the time noq's pacer would have
  released it, tested under both `etf` and `fq`. The socket is shared across
  connections per worker, so `SO_MAX_PACING_RATE` cannot express
  per-connection pacing. Measure on a real NIC as well as loopback, since
  `etf` needs hardware offload to be exact.

Report CPU per Gbps, RSS, loss, and p99 latency via `just bench BASE` on
Linux. The verdict names which levers ship and which qdisc, if any, relay
hosts need.

## Related

- [#3201](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) -
  zero-copy on the same trains
- [#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) -
  the UDP-side pool the same buffers could feed
