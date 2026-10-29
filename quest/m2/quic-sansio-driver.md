# [L] A runtime-free QUIC driver

## Goal

A caller with its own event loop and UDP socket runs moq sessions over moq-quic, with no
runtime in the library. The driver owns moq-quic's endpoint, its connections, and TLS. The
caller hands it each datagram it receives, sends each one the driver returns, and polls it
on each wake and at the deadline the poll returns. Its connections implement moq-net's
`transport::poll::Session`, so moq-net's `Client::connect` and `Server::accept` take them.
No tokio or io_uring is in its dependency tree.

The first consumer is [kixelated/rust2](https://github.com/kixelated/rust2)'s exporter,
whose C, C++, and Kotlin bindings drive it from their own loops
([sessions on sans-IO QUIC](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-export/session-quic.md)).

## Plan

Decided with the maintainer on 2026-10-28, from
[kixelated/rust2#215](https://github.com/kixelated/rust2/pull/215):

- **After [waits](/quest/m1/quic-api/waits.md) and [poll](/quest/m1/quic-api/poll.md).**
  Once moq-quic owns its waits, a driver is I/O, timers, and a lock. This one is the
  timers and the lock, with the caller doing the I/O. Built earlier, it would carry the
  waiter tables that waits deletes.
- **Pull, like moq-net's `Driver`s.** The caller passes in each received datagram with its
  source address. It polls with its `kio::Waiter` on each wake, after each datagram, and
  at the deadline the poll returns, then drains outgoing datagrams, each with its
  destination, until none is left. The driver makes no calls back into the caller, and
  hands out owned bytes, since a binding can't lend a mutable buffer across a C ABI. A
  transmit into a caller's buffer, such as a GSO batch, can follow on
  [the QUIC I/O boundary](/quest/m2/quic-io-boundary.md).

Open, for whoever starts it:

- Where it lives: a module in moq-quic, which doesn't depend on moq-net today, or a crate
  of its own.
- Whether it ships with WebTransport over HTTP/3, sharing
  [native WebTransport](/quest/m1/quic-api/native-web.md)'s `quic::web`, or with raw
  QUIC's moq ALPN first.
- How it builds its TLS configs (identity, roots, fingerprint pinning) without
  moq-tokio's `tls` module.

Mirror the two drivers' names and signatures where they overlap. Test it against
moq-tokio's server over loopback on simulated time. Bound a hostile peer's cost as the
other drivers do.

Public API: a new driver, its endpoint and connection types, and their `poll::Session`
impl.

## Required

- [Waits](/quest/m1/quic-api/waits.md) - moq-quic owns every wait, so the driver keeps no waiter table
- [Poll forms](/quest/m1/quic-api/poll.md) - the drivers' poll shape this one mirrors

## Related

- [UDP demux](/quest/m2/p2p/udp-demux.md) - also feeds datagrams from a socket the endpoint doesn't own
