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
- Decided 2026-11-01 (maintainer, socket planning interview): an `Endpoint`
  wraps [sockets](/quest/m2/socket/rust.md). A service is a `Listener` at
  `mesh/<id>/<service>`, `connect` dials it with the caller's own endpoint ID,
  and `accept` reads the remote endpoint ID from the `.dial/<id>` segment,
  which the grant pins. Extend `moq auth`'s mesh grant with publish on
  `mesh/*/*/.dial/<id>`, and update `doc/bin/relay/auth.md`.
- Streams are [byte streams](/quest/m2/stream/rust.md) at the protection
  level the service declares: identity reads `moq-net` directly, and
  authentication, the mesh default, reads through `moq-sign` against the
  `PublicKey` parsed from the path it subscribes to: the listener's
  `mesh/<id>/` for a dialer, and the `.dial/<id>` segment for a listener.
- The secret key comes from `moq_sign::SecretKey::load_or_generate`.
- The [daemon](/quest/m2/mesh/daemon.md) is built on this API, so a process
  either embeds an `Endpoint` or talks to the local daemon's relay.
- Write a short porting note into the [guide](/quest/m2/mesh/guide.md).

## Required

- [Rust sockets](/quest/m2/socket/rust.md) - the dial and accept this wraps
- [Rust byte streams](/quest/m2/stream/rust.md) - the stream this API returns
