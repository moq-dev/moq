# [M] Endpoint API

## Goal

A Rust application builds an `Endpoint` from its secret key and a mesh
enrollment, then `connect(endpoint_id, service)` opens a byte stream to that
endpoint's named service and `accept()` yields incoming ones with the remote
endpoint ID verified. The surface mirrors iroh's (`Endpoint`, `id()`,
`connect`, `accept`, bi streams, `remote_id()`) so an iroh application ports
with small changes, without iroh's wire or discovery.

## Plan

Decided 2026-10-25 (maintainer): iroh-shaped, not drop-in compatible; tickets,
`EndpointAddr`, and `Router` are out unless porting proves they are needed.

- A service is the iroh ALPN analog: a name under `mesh/<id>/`.
- Streams are [byte streams](/quest/m2/stream/rust.md) at the protection
  level the service declares: identity reads `moq-net` directly, and
  authentication, the mesh default, reads through `moq-sign` against the
  `PublicKey` parsed from the `mesh/<id>/` path it subscribes to.
- The secret key comes from `moq_sign::SecretKey::load_or_generate`.
- The [daemon](/quest/m2/mesh/daemon.md) is built on this API, so a process
  either embeds an `Endpoint` or talks to the local daemon's relay.
- Write a short porting note into the [guide](/quest/m2/mesh/guide.md).

## Required

- [Rust byte streams](/quest/m2/stream/rust.md) - the stream this API returns
