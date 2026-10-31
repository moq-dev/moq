# [XS] tls:// sessions report their negotiated ALPN

## Goal

A session accepted on a `tls://` listener reports the negotiated moq ALPN in
`Link::alpn`, as raw QUIC, WebTransport, and WebSocket sessions already do,
so admission and logs see the protocol on every transport.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7358](https://github.com/moq-dev/moq/pull/7358): m2, as
recommended. [moq-dev/moq#7358](https://github.com/moq-dev/moq/pull/7358) (open) adds client certificates and SNI on `tls://`
and left `link.alpn` `None` there as out of scope; build on it.

The TCP/TLS accept loop (`spawn_tcp_loop`, `rs/moq-tokio/src/server.rs`
around `:1474` on origin/main) builds its `Link` with only `remote` and
`local`. Read the ALPN from the completed rustls handshake and set `alpn`.
Plain `tcp://` stays `None`. Test with a `tls://` session asserting the
ALPN in `Request::link`.

Public API: none (a field that was `None` is now set). Wire: none.
