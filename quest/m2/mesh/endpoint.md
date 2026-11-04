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
- Decided 2026-11-01 (maintainer, socket planning interview), and updated
  2026-11-03 when sockets became endpoints: an `Endpoint` wraps a
  [socket](/doc/concept/socket.md) endpoint. Settle the layout here: such as
  one socket endpoint per service at `mesh/<service>/.tcp/<id>`, or one per
  node with the service named in the session. `connect` dials with the
  caller's own endpoint ID, and `accept` reads the remote ID from the dial,
  which the socket's answer check proves and the grant pins (publish
  `.tcp/<id>` only). Update `moq auth`'s mesh grant and
  `doc/bin/relay/auth.md` to match.
- Streams are [byte streams](/quest/m2/stream/rust.md) at the protection
  level the service declares: identity reads `moq-net` directly, and
  authentication, the mesh default, reads through `moq_crypto::sign` against the
  `PublicKey` parsed from the peer's endpoint id, which names the endpoint
  broadcast it subscribes to.
- The secret key comes from `moq_crypto::sign::SecretKey::load_or_generate`.
- A mesh daemon (Kixel's, outside this repository, decided 2026-11-02) is
  built on this API, so a process either embeds an `Endpoint` or talks to a
  local [mesh relay node](/quest/m2/mesh/node.md).
- Write a short porting note into the [guide](/quest/m2/mesh/guide.md).

## Required

- [Rust byte streams](/quest/m2/stream/rust.md) - the stream this API returns
