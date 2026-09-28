# [XS] iroh honors the configured versions

## Goal

An `iroh://` client or listener configured with `moq-lite-07-wip` negotiates
it, as `https://`, `moqt://`, TCP, and Unix sockets already do. A version
list that iroh cannot offer is refused at startup, never silently replaced.

## Plan

https://github.com/moq-dev/moq/pull/4148 took `moq-lite-07-wip` out of
`moq_net::ALPNS` so it is opt-in only, but `rs/moq-tokio/src/iroh.rs` builds
its listener ALPNs, its dial offers, and its H3 subprotocols from that
constant rather than the configured `Versions` (`versions.alpns()`, which
`noq.rs`, `server.rs`, and `client.rs` use). So the opt-in is accepted by
config and then ignored on iroh. Thread the configured versions through
instead.

`moq-ffi`'s transport (`rs/moq-ffi/src/transport.rs`) also offers
`moq_net::ALPNS` directly; fix it here too. The relay's WebSocket listener
already honors the configured versions. Scope is those two files.

Decided by the maintainer: the finalized lite-07 ALPN stays `moq-lite-07`,
as `drafts/draft-lcurley-moq-lite.md` already says. Peers from the yanked
0.3.2 / 0.15.3 releases advertise `moq-lite-07` with an older framing, and
could land on it with a finalized peer; the maintainer accepts that risk
rather than burn the identifier. Nothing in this quest changes the ALPN.

## Related

- [iroh opt-in for moq-relay](/quest/m1/relay-iroh-opt-in.md) - a separate change: whether the relay builds iroh at all
