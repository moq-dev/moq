# [L] Redesign the QUIC I/O boundary around io_uring

## Goal

`moq-quic`'s sans-IO boundary lets moq-uring receive from the kernel's
buffer ring and transmit into caller-owned buffers, with no copy between the
kernel and the crypto. This quest owns the transmit contract; registering
those buffers with the ring is
[#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md)'s. The API may break; it is in-tree.

## Plan

Decided in the 2026-09-30 plan: commit to the redesign, but start only after
a `moq-quic` profile shows where ingress and egress spend their time
(`just bench-profile cpu` on `io-uring-workers`).
No current profile is from `moq-quic`, and moq-uring already avoids every
other copy the current API allows.

Candidates, to be ranked by the profile:

- `Endpoint::handle` accepts a foreign-owned buffer (a buffer-ring slot), and
  stream frames keep references to it instead of requiring an owned
  `BytesMut`.
- `poll_transmit` writes into a caller-owned slice as a contract, not by
  relying on `Vec` capacity.
- One `handle` call takes a whole GRO batch.

Received datagrams and leased stream bytes are copied out of their receive
buffer, since a slice keeps the whole batch alive while the receive budgets
charge only its bytes. A frame that references a ring slot must keep that
bound. Per-packet ownership could let large frames skip the copy, which costs
up to 70% of `wire_decode` (16 datagrams a packet) and 20% of `receive_lease`
under the benches' plaintext crypto.

The receive path copies at several seams, listed in the 2026-10-27 audit,
and removing one in isolation can leave a small retained slice pinning a
whole GRO allocation or an unavailable ring slot. Profile copied bytes across
all of them together, sweeping payload size × fragments × concurrent readers,
including a stalled reader:

- moq-uring's `Endpoint::demux` copies each completion out of the receive
  pool into a `BytesMut`.
- moq-tokio's endpoint receive copies each batch out of its reusable iovecs.
- moq-quic's lease `extend` copies out of datagram backing to bound what a
  slice retains (deliberate), and `fill` builds a temporary `Vec` of chunks
  per call.
- A partial MoQ frame copies into canonical frame storage, which
  `Segment::new` zero-fills first
  ([streaming frame read owners](/quest/m2/perf/frame-read-owners.md) owns
  that fill). Whole-frame `Bytes` writes skip this copy.

Decided 2026-10-08: `SENDMSG_ZC`
([#3201](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md))
and registered TX buffers
([#3204](/quest/m3/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md))
Require this quest and build on its transmit contract, so the contract is
settled once; registering buffers stays with #3204, which measures whether
it pays. Report relay CPU per Gbps before and after on
the same workloads.

## Required

- [Relay egress profile](/quest/m2/quic-egress-profile.md) - its moq-quic profile ranks this quest's candidates, and its send-buffer pool shares the `SendBuffer` seam
