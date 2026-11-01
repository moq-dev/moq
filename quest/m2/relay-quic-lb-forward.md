# [L] moq-relay forwards QUIC packets by QUIC-LB server ID

## Goal

A moq-relay QUIC listener with a QUIC-LB server ID and a peer map forwards
every short-header packet whose connection ID names another server to that
server's unicast address, carrying the client's address. The owning relay
processes it as if it came from the client, and sends that connection's
packets with a configured return source address (direct server return), so
an established connection survives landing on the wrong instance behind an
anycast IP or a load-balancing VIP. Long-header packets are never forwarded:
the handshake relies on the front end keeping a flow on one instance. The
flags are documented inline, with MIGRATE notes.

## Plan

Settled with the maintainer, 2026-10-30, for moq.pro's anycast edge cluster
on Cloudflare:

- Why: Chrome's WebTransport never follows `preferred_address`. Chromium
  sends every `https` WebTransport through `DedicatedWebTransportHttp3Client`,
  a bare `QuicSpdyClientSession` with no migration path, while its pooled
  HTTP/3 client validates the server preferred address. Measured locally
  against a moq-relay advertising `--listen-preferred-v4` (Chrome 156: the
  net-log shows the address received and never acted on; a forced-QUIC
  `fetch()` sends `PATH_CHALLENGE` to it). moq-cli does not migrate either.
  So a browser stays on the anycast or VIP address for a connection's life,
  and a route change or a VIP rebalance lands its packets on another
  instance.
- Scope: established connections only. A handshake split across instances
  fails and the client retries. Rejected: a Retry carrying the server ID
  (one extra round trip per connection) and a consistent hash of the
  client's ID (needs an identical membership view everywhere).
- Connection IDs: the existing plaintext QUIC-LB generator (`listen.lb.id`,
  `listen.lb.nonce`, [#848](https://github.com/moq-dev/moq/pull/848)), not the
  draft's encrypted form.
- Return path: direct server return. The owner sends a forwarded
  connection's packets with the configured return address (the anycast or
  VIP address) as source, so only the client-to-server direction takes the
  extra hop. Whether the platform allows that source is the deployment's
  concern; moq.pro asks Cloudflare.
- Peer map: server ID to unicast address, updated at runtime (the deployment
  feeds it, moq.pro from its own API), through config the relay hot-reloads
  or the library API; the relay refuses a forward to an unknown ID.

Open for implementation:

- The forwarding encapsulation: the original client address and the
  address the packet arrived on, prepended to the datagram and sent to the
  peer's QUIC port or a dedicated one. Forwards are accepted only from
  peer-map addresses and authenticated (a shared key), since a forged
  client address would let anyone inject packets or reflect traffic.
- Sending from the return address: a socket that may use a non-local
  source (`IP_FREEBIND` or `IP_TRANSPARENT` on Linux) versus binding the
  return address where the platform routes it.
- Tests: three relays on loopback addresses, a connection established on
  one, its short-header packets delivered to another, and the session
  continuing.

Interactions: per-core shard steering already refuses QUIC-LB connection
IDs (`ShardWithQuicLb`), so if
[Shard the relay by default](/quest/m2/perf/shard-default.md) flips the
default, a listener with `lb.id` stays unsharded. The io_uring listener's
`lb.id` refusal ([io_uring package](/quest/m2/relay-io-uring-package.md))
can stand: forwarding lives on the tokio backend unless a consumer asks.

Public API: new listener settings (peer map, return address, forward key)
and a library entry point to update the peer map. Wire: none toward clients;
the peer-to-peer forward format is internal.
