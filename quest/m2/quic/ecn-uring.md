# [S] ECN on the io_uring UDP path

## Goal

A relay on the io_uring runtime keeps ECN negotiated the way it already does
on the tokio runtime: its packets leave marked ECT(0), and marks on arriving
packets reach the QUIC stack, so the peer's ACKs carry ECN counts and noq
never disables ECN on a uring-to-uring session. A viewer's session is
unaffected.

## Plan

noq-proto already does classic ECN end to end: every path starts with
`sending_ecn` on and marks ECT(0), `process_ecn` validates the peer's ACK
ECN counts, and an ACK without counts disables it. `noq-udp` carries the
mark through `IP_TOS` / `IPV6_TCLASS` on send and reads it back on receive
(`unix.rs:607-620`).

The gap is ours. `rs/moq-uring/src/udp.rs` builds its own control messages
and carries only `UDP_SEGMENT` on send and `UDP_GRO` on receive: no TOS or
TCLASS out, no `IP_RECVTOS` / `IPV6_RECVTCLASS` in. Nothing in `moq-uring`
mentions ECN, and `udp::TxBuf::send(len, to, segment)` has no slot for the
codepoint, so on the io_uring path the peer never sees a mark, its ACKs
carry no counts, and noq turns ECN off within the first ACK.

- Send: `TxBuf::send` takes the codepoint beside the segment size and writes
  `IP_TOS` (v4) or `IPV6_TCLASS` (v6) into the same control buffer as
  `UDP_SEGMENT`, so a GSO train carries the mark on every segment. The noq
  and quinn adapter passes `Transmit::ecn` at both call sites,
  `rs/moq-uring/src/quic/quinn/endpoint.rs:377` and
  `rs/moq-uring/src/quic/quinn/connection.rs:797`. quiche has no ECN send
  API, so its two callers (`rs/moq-uring/src/quic/quiche/endpoint.rs:476`
  and `rs/moq-uring/src/quic/quiche/connection.rs:843`) pass no codepoint
  and change only to match the signature.
- Receive: enable `IP_RECVTOS` and `IPV6_RECVTCLASS` on the socket and parse
  the TOS or TCLASS control message next to `UDP_GRO`. `udp::Packet` gains
  an `ecn` accessor (one mark per completion; a GRO batch shares it), and
  the noq and quinn adapter threads it into both `Endpoint::handle` calls at
  `quinn/endpoint.rs:240-252`, which pass `None` today.
- `moq-uring` is 0.0.1 and unpublished, so the `udp` module's signature
  changes on `main`. No config, wire, or doc changes.
- Regression, in `rs/moq-uring/tests`: a socket pair through the worker
  sends with ECT(0) and the receiving `Packet::ecn` reads it back, over v4
  and v6, for a single datagram and for a GSO train; and a datagram sent
  with CE arrives as CE. Fails today. noq-proto 1.2 exposes no
  ECN state, so the session-level check waits for the fork; the `rs uring`
  nightly lane already runs this crate and gates on the kernel floor.

## Related

- [Measure ECN on the backbone](/quest/m2/quic/ecn-measure.md) - the study
  this fix unblocks
- [L4S on the backbone](/quest/m2/quic/ecn.md) - the fork-side half
