# [S] A listener takes an optional client certificate from any issuer

## Goal

A moq-tokio listener can ask clients for an optional certificate from any
issuer, self-signed included. It verifies only that the client holds the
certificate's key, and the accepted request reports the certificate's
fingerprint as its peer identity (the same value `tls::Identity::fingerprint`
gives on the dialing side). A client that presents no certificate connects as
it does today. Every TLS listener that shares `Listen::client_auth` (QUIC,
`tls://` streams, and the HTTPS port's `tls://`) behaves the same.

Non-goals: deciding trust (the application judges the fingerprint), CLI or
config-file exposure, and changing the CA-rooted `root` mode.

## Plan

Decided 2026-11-17 (maintainer, kixel.ai's /quest-run, planning a follow-up
of [kixel-ai/kixel.ai#122](https://github.com/kixel-ai/kixel.ai/pull/122)):
✅ an optional accept-any mode (rejected: keeping pinned `peers` and adding a
non-mandatory switch, which [moq-dev/moq#8957](https://github.com/moq-dev/moq/pull/8957)
deletes for want of a caller and which would make Kixel copy its mesh
membership into a TLS allowlist; building the listener's rustls config in
Kixel instead, which forks transport setup).

The consumer is Kixel's mesh relay node: its LAN links would present the
node's `tls::Identity` through `Connect::identity`, and its admission would
read the listener's fingerprint from the request instead of trusting the
dialer's claim ([kixel.ai's LAN mTLS quest](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m1/fleet/lan-mtls.md)).
The node's one listener also serves its CDN uplinks and local clients, which
present no certificate, so pinned `peers`, whose handshake refuses them, cannot
serve it.

Recommended, open to the implementer: a `#[serde(skip)]` field on `Listen`
decided in `Listen::client_auth`, the one place the listeners agree on mTLS;
refuse it combined with `root`, as pinned peers refused `root`, since one
would be silently ignored; and refuse it on quiche like the other in-memory
modes. Land it after #8957, which rewrites the same code. Tests: a
self-signed client reports its fingerprint, a client with none still
connects, and a client that cannot prove the key fails the handshake.

Public API: additive.
