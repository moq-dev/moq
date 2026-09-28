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

The relay's WebSocket listener and `moq-ffi`'s transport also read
`moq_net::ALPNS` directly. Check whether they have the same hole; fix them
here if it is the same small change, otherwise report it.

Decided by the maintainer: the finalized lite-07 ALPN stays `moq-lite-07`,
as `drafts/draft-lcurley-moq-lite.md` already says. Peers from the yanked
0.3.2 / 0.15.3 releases advertise `moq-lite-07` with an older framing, and
could land on it with a finalized peer; the maintainer accepts that risk
rather than burn the identifier. Nothing in this quest changes the ALPN.

## Related

- [iroh opt-in for moq-relay](/quest/m1/relay-iroh-opt-in.md) - a separate change: whether the relay builds iroh at all
