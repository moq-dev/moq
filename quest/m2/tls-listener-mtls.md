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
without it.

Decided by the maintainer (2026-10-06): the peer certificate comes from a
new accessor on qmux's TLS session upstream (qmux is in our org), then a
dependency bump here. Rejected: owning the TLS accept in this repository, or
leaving it open.

Keep the token path working on the same listener, and keep pinned peers and
CA roots meaning the same as on QUIC.

[Relay client-CA validation](/quest/m1/relay-auth-client-ca.md) refuses a
listener TLS client CA on a stream-only relay (the `NoBackend` case in
`rs/moq-relay/src/relay.rs`), since nothing checks it there. Once a `tls://`
listener verifies the certificate, this quest lifts that refusal for a relay
with a `tls://` listener and updates the refusal's test.

Public API: relay config may gain a listener option. Wire: none.

## Required

- [Relay client-CA validation](/quest/m1/relay-auth-client-ca.md) - adds the stream-only client-CA refusal this lifts for `tls://`
