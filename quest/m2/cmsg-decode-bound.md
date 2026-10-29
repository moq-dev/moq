# [S] Bound cmsg payload reads and keep every receive cmsg

## Goal

On the moq-sock (Tokio) runtime, a received datagram keeps every control
message its socket options ask for, including the ECN mark, and a truncated
or short control message is never read past its end:

- `CMsg::decode` in `rs/moq-sock/src/udp/cmsg/mod.rs` refuses a control
  message shorter than its type instead of reading `size_of::<T>()` bytes
  past it in release builds.
- `cmsg::LEN` fits every receive option set the socket enables, on every
  platform.
- The normal `recvmsg` and `recvmmsg` paths in `rs/moq-sock/src/udp/unix.rs`
  handle `MSG_CTRUNC` instead of silently using partial metadata.

## Plan

- Today only a `debug_assert_eq!` checks the length;
  [moq-dev/moq#6678](https://github.com/moq-dev/moq/pull/6678) bounded the
  iterator but not `decode`. Return `None` when the payload is short and let
  the Unix and Windows callers skip it.
- From the 2026-10-28 external security audit, checked at
  [138a27d209](https://github.com/moq-dev/moq/commit/138a27d209): on Linux
  the socket enables `IP_PKTINFO` (or `IPV6_RECVPKTINFO`), `IP_RECVTOS` (or
  `IPV6_RECVTCLASS`), `UDP_GRO`, and `SO_TIMESTAMPNS`, which need about
  112 bytes of control space for IPv4 and 120 for IPv6. `cmsg::LEN` is 96,
  and the startup assert in `unix.rs` checks only a subset. The audit's
  loopback probe on Linux 7.0 got `MSG_CTRUNC` with the timestamp, GRO, and
  packet info present and the ECN mark omitted, so CE marks never reach
  moq-quic on this runtime. moq-uring sizes its own buffer per family
  (`CONTROL_LEN` in `rs/moq-uring/src/udp.rs`), carries no timestamp, and
  drops a `MSG_CTRUNC` receive, so it is unaffected. Size `cmsg::LEN` the
  way its `CONTROL_LEN` is sized, as a compile-time `cmsg_space` sum, but
  over this socket's own option set per target and family, timestamp
  included; `CONTROL_LEN` itself is too small here.
- On `MSG_CTRUNC` despite the sizing, keep the datagram and log once rather
  than drop it. Losing metadata beats losing the packet.
- A Linux test enables every option and sends an ECT(0) GSO train
  (`UDP_SEGMENT`) that the receiver coalesces, since a lone datagram brings no
  GRO message and pktinfo, timestamp, and ECN alone (88 bytes for IPv4, 96
  for IPv6) already fit today's buffer. Over both `recvmsg` and `recvmmsg`,
  it asserts no `MSG_CTRUNC` and that `RecvMeta` carries the GRO stride, the
  ECN mark, a timestamp, the destination address, and the interface index,
  for IPv4 and IPv6.

Decided 2026-10-28 (maintainer): one quest for the bound, the sizing, and
`MSG_CTRUNC`, since they're the same decoder and buffer.

Public API: none. Wire: none.

## Related

- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - fuzzes the same cmsg walk
- [Measure ECN on the backbone](/quest/m2/quic-ecn-measure.md) - runs on io_uring; a moq-sock run would see no CE until this lands
