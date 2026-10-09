# [L] Redesign the QUIC I/O boundary around io_uring

## Goal

`moq-quic`'s sans-IO boundary lets moq-uring receive from the kernel's
buffer ring and transmit into caller-owned buffers, with no copy between the
kernel and the crypto. This quest owns the transmit contract; registering
those buffers with the ring is
[#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md)'s. The API may break; it is in-tree.

## Plan

Decided in the 2026-09-30 plan: commit to the redesign, but start only after
a noq or `moq-quic` profile shows where ingress and egress spend their time.
No current profile is from noq, and every other copy is removed without an
API change by [Remove moq-uring copies](/quest/m1/perf/uring-copies.md).

Candidates, to be ranked by the profile:

- `Endpoint::handle` accepts a foreign-owned buffer (a buffer-ring slot), and
  stream frames keep references to it instead of requiring an owned
  `BytesMut`.
- `poll_transmit` writes into a caller-owned slice as a contract, not by
  relying on `Vec` capacity.
- One `handle` call takes a whole GRO batch.

Decided 2026-10-08: `SENDMSG_ZC`
([#3201](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md))
and registered TX buffers
([#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md))
Require this quest and build on its transmit contract, so the contract is
settled once; registering buffers stays with #3204, which measures whether
it pays. Report relay CPU per Gbps before and after on
the same workloads.

## Related

- [Relay egress profile](/quest/m2/quic-egress-profile.md) - its send-buffer pool lever touches the same `SendBuffer` seam

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the redesign breaks `moq-quic`'s API
- [Relay profiling recipe](/quest/m1/perf/lock-profile.md) - the captures that rank the candidates
- [Remove moq-uring copies](/quest/m1/perf/uring-copies.md) - the cheap wins land first, so the profile shows what is left
