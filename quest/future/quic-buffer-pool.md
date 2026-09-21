# [S] Send buffer pools

## Goal

A measured verdict on pooled send buffers versus `Bytes` in the stream send
path. noq's `SendBuffer` keeps a write above 1452 bytes as the caller's
`Bytes` without copying and coalesces smaller writes into a `BytesMut`;
moq-net hands it frames as `Bytes` from its own allocation. Either a pool of
fixed-size buffers recycled after acknowledgment measurably cuts allocation
and cache misses on the relay's egress, or `Bytes` is shown to be within
noise and the idea is closed.

## Plan

- Count allocations and bytes copied per frame on the fanout and video
  shapes with the existing profiling captures, split by frame size, so the
  share of sub-threshold copies is known before anything is built.
- Prototype in the fork: a `BufFactory`-style seam on `SendBuffer` that takes
  buffers from a pool and returns them on ACK, with the pool sized per
  connection from the send window.
- `just bench BASE` on Linux: CPU per Gbps, RSS, and p99 latency. Ship only a
  measured win.

## Required

- [Fork noq](/quest/next/quic/fork.md) - the seam lives there

## Related

- [#3204](/quest/next/perf/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) -
  the UDP-side pool the same buffers could feed
