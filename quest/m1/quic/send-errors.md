# [S] Surface UDP send errors so the client can fall back

## Goal

When a UDP send fails because the address family is unreachable (for
example an IPv6 target on a host without IPv6 routing), moq-tokio's QUIC
endpoint reports the error instead of swallowing it, and moq-tokio's dial race drops
that QUIC attempt at once rather than waiting out the handshake timeout.

## Plan

Rebase [kixelated/quinn#3](https://github.com/kixelated/quinn/pull/3) onto
`moq_sock::udp` and moq-tokio's imported quinn layer. It is 154 commits behind quinn main and
never went upstream; it touches only the udp and tokio crates, not the
sans-IO core. Decided 2026-09-30: an m1 quest after the switch, since the
client connect path is user-facing.

Only fatal, destination-specific errors end the attempt; transient ones
(`ENOBUFS`, `EAGAIN`) stay handled as packet loss, as today. Test the fallback with a dial
against an unroutable family, and check that moq-uring maps the same errors
through its ring.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the change lands in `moq_sock::udp` and moq-tokio's imported quinn layer

## Related

- [quinn#2766](https://github.com/quinn-rs/quinn/issues/2766) - the upstream issue
