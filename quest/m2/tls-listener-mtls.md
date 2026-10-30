# [M] tls:// peers can authenticate by certificate

## Goal

A relay's `tls://` (qmux over TLS on TCP) listener can identify a cluster
peer by its client certificate, as the QUIC listener does, so an upstream
link doesn't need a token.

## Plan

#4816 added `tls://` with no client certificate request, so peers present a
token. `moq_tokio::tcp::Listener::with_tls` (`rs/moq-tokio/src/tcp.rs`)
already takes an `Arc<rustls::ServerConfig>`, so requesting a certificate is
config. The real gap is getting the peer certificate out afterwards:
`qmux::tls::Server::accept` finishes the handshake and returns a `Session`
without it. The same accessor also returns the SNI the client sent, so a
`tls://` session reports it as QUIC sessions do (from
[moq-dev/moq#6509](https://github.com/moq-dev/moq/pull/6509), decided
2026-10-23).

Decided by the maintainer (2026-10-06): the peer certificate comes from a
new accessor on qmux's TLS session. Rejected: owning the TLS accept outside
qmux, or leaving it open. Decided 2026-10-20 (maintainer): no new work in
moq-dev/web-transport, so the accessor goes on the in-tree qmux crate (`rs/qmux`).

Keep the token path working on the same listener, and keep pinned peers and
CA roots meaning the same as on QUIC.

A stream-only server refuses a listener client CA or pinned `tls.peers` (the
`MtlsUnsupported` case in `rs/moq-tokio/src/server.rs`), since nothing checks
them there. Once a `tls://` listener verifies the certificate, this quest lifts
that refusal for a server with a `tls://` listener and updates the refusal's
test.

Decided 2026-10-08: request the certificate optionally, never require it,
and only on the moq-ALPN TLS config, so HTTP and RTMPS clients on the shared
HTTPS port are never asked for one. That port already serves `tls://`: the
relay's web acceptor picks `moq_tokio::tcp::TlsAcceptor::config` (built like
the `listen.tcp` TLS config, by `TcpTls::new` in `rs/moq-tokio/src/server.rs`)
from the ClientHello, so the optional request goes on that config too. There
the relay holds the `rustls::ServerConnection` (`listener::Stream::tls`), so it
reads the peer certificate itself and `TlsAcceptor::accept` needs a way to
carry it into the `Request`, as `websocket::Options::identity` does.

Public API: relay config may gain a listener option. Wire: none.

## Related

- [qmux drives its own socket](/quest/m2/perf/stream/driver.md) - replaces qmux's reader and writer tasks and its constructors' return type
