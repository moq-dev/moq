# [XS] tls:// sessions report their negotiated ALPN

## Goal

A session accepted on a `tls://` listener reports the negotiated moq ALPN in
`Link::alpn`, as raw QUIC, WebTransport, and WebSocket sessions already do,
so admission and logs see the protocol on every transport.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7358](https://github.com/moq-dev/moq/pull/7358): m2, as
recommended. [moq-dev/moq#7358](https://github.com/moq-dev/moq/pull/7358) (merged) added client certificates and SNI on `tls://`
and left `link.alpn` `None` there as out of scope; build on it.

[moq-dev/moq#7362](https://github.com/moq-dev/moq/pull/7362) (merged) already sets
the ALPN for `tls://` sessions on the relay's shared TCP port
(`rs/moq-tokio/src/tcp.rs:299-312`, `Incoming::tcp(..).with_alpn(alpn)`).
A `tls://` listener's own accept loop (`spawn_tcp_loop`,
`rs/moq-tokio/src/server.rs:1513`) still builds `Incoming::tcp` without it.
Read the ALPN from the completed rustls handshake there the same way, sharing
the read with `tcp.rs` rather than copying it.
Plain `tcp://` stays `None`. Test with a `tls://` session asserting the
ALPN in `Request::link`.

Public API: none (a field that was `None` is now set). Wire: none.
