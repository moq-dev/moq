# [L] Redesign the QUIC I/O boundary around io_uring

## Goal

`moq-quic`'s sans-IO boundary lets moq-uring receive from the kernel's
buffer ring and transmit into registered buffers, with no copy between the
kernel and the crypto. The API may break; it is in-tree.

## Plan

Decided in the 2026-09-30 plan: commit to the redesign, but start only after
a noq or `moq-quic` profile shows where ingress and egress spend their time.
No current profile is from noq, and every other copy is removed without an
API change by [Remove moq-uring copies](/quest/m1/perf/uring-copies.md).

Candidates, to be ranked by the profile:

- `Endpoint::handle` accepts a foreign-owned buffer (a buffer-ring slot), and
  stream frames keep references to it instead of requiring an owned
  `BytesMut`.
- `poll_transmit` writes into a caller slice or registered buffer as a
  contract, not by relying on `Vec` capacity.
- One `handle` call takes a whole GRO batch.

Registered TX buffers ([#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md))
and `SENDMSG_ZC` ([#3201](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md))
build on the transmit contract. Report relay CPU per Gbps before and after on
the same workloads.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the redesign breaks `moq-quic`'s API
- [Relay profiles](/quest/m1/performance-profiles.md) - the redesign targets measured costs
- [Remove moq-uring copies](/quest/m1/perf/uring-copies.md) - the cheap wins land first, so the profile shows what is left
