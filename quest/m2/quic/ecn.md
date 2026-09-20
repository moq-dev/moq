# [M] ECN on the backbone

## Goal

Relay-to-relay sessions keep ECN negotiated on the io_uring runtime the way
they already do on the tokio runtime, and a marking network reduces the rate
before a queue overflows instead of after. L4S (ECT(1) with a scalable
response) is measured behind an option. Paths that strip or mangle the marks
fall back to no ECN, and a viewer's session is unaffected.

## Plan

noq-proto already does classic ECN end to end: every path starts with
`sending_ecn` on and marks ECT(0) (`connection/paths.rs:305`,
`connection/mod.rs:1260`), `process_ecn` validates the peer's ACK ECN counts
and an ACK without counts disables it (`:3146`, `:3101`), and a CE increase
reaches `Controller::on_congestion_event(is_ecn = true)`, which Cubic and
BBR3 both handle. `noq-udp` carries the mark through `IP_TOS` /
`IPV6_TCLASS` on send and reads it back on receive (`unix.rs:607-620`).

The gap is ours. `rs/moq-uring/src/udp.rs` builds its own cmsgs and carries
no TOS or TCLASS on send and no `IP_RECVTOS` / `IPV6_RECVTCLASS` on receive,
so on the io_uring path the peer never sees a mark, its ACKs carry no counts,
and noq turns ECN off within the first ACK.

- moq-uring: set the codepoint from `Transmit::ecn` in the send cmsg beside
  the GSO segment size, enable the receive-side TOS/TCLASS cmsg, and fill
  `RecvMeta::ecn`, so the io_uring path matches `noq-udp`. Regression: a
  uring-to-uring session reports ECN still enabled after the handshake.
- In the fork: an `Ect1` marking option and the accounting to keep the two
  codepoints apart, so an L4S response (proportional to the CE fraction per
  RTT, per RFC 9330 to 9332) can be tried as a `Controller` change without a
  transport change. Off by default.
- `moq-tokio`'s `[quic]` section gains `ecn = off | ect0 | ect1`, ect0 by
  default (today's behavior on tokio), so a deployment whose network mangles
  marks can turn it off explicitly.
- Measure on a netem bottleneck with a marking qdisc (`fq_codel`, then
  `dualpi2` for L4S) against the same bottleneck dropping: queueing delay,
  goodput, loss. Record whether Linode's and OVH's networks preserve the
  marks between relays; if neither does, L4S stays off and the result is
  written down.

## Required

- [Fork noq](/quest/m2/quic/fork.md) - the `Ect1` option lives there
