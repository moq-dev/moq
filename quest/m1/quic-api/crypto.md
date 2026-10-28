# [M] moq-quic's crypto is rustls, not a plugin

## Goal

moq-quic's crypto traits are crate-private, its configs take rustls types,
and a connection reports its peer and handshake as concrete types: no
`Box<dyn Any>` for a caller to downcast, and `transport_parameters` is
private.

## Plan

Decided 2026-10-23 (maintainer): seal to rustls. `crypto::Session`,
`ClientConfig`, `ServerConfig`, and the key traits have one real
implementation; the plaintext one serves the fuzz target and benches and
stays behind the `fuzz` feature.

- `peer_identity()` returns `Option<Vec<CertificateDer<'static>>>`.
- `handshake_data()` returns a concrete struct (negotiated ALPN, server
  name), replacing the downcasts to rustls `HandshakeData` in moq-tokio's
  backend and moq-uring's handshake.
- Client and server configs are built from `Arc<rustls::ClientConfig>` and
  `Arc<rustls::ServerConfig>` directly; `QuicClientConfig`/
  `QuicServerConfig` and their `TryFrom` go private if nothing else needs
  them.
- `pub mod transport_parameters` goes private; only the trait signatures
  needed it.

What it deletes, from the 2026-10-23 consumer audit: the `!Send`-scoped
`handshake_data().downcast().unwrap()` in moq-tokio's backend,
`PeerIdentity::from_any` in moq-tokio's `tls.rs` and its tests, `quic::web`'s
`OnceLock` that caches the negotiated ALPN only because the downcast
allocates, moq-uring's `peer_chain` returning `Vec<Vec<u8>>` that
moq-relay's `uring.rs` converts back to `CertificateDer`, the
`QuicClientConfig`/`QuicServerConfig::try_from` calls with their
`NoInitialCipherSuite` handling or `unwrap` (moq-tokio's backend and web
client and server, moq-uring, moq-relay's `cluster.rs`), and about 18
`#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]` gates in `quic::web`
that a crypto-provider-agnostic surface no longer needs.

Mirror the accessors in both drivers. The Impact bullet says how
to migrate in moq-quic, moq-tokio, and moq-uring.
