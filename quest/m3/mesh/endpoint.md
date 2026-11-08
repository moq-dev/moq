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

- A service is the iroh ALPN analog: a socket endpoint per service name
  (below), while each node's address and status tracks stay under its
  `mesh/node/<id>/`.
- Decided 2026-11-01 (maintainer, socket planning interview), and updated
  2026-11-03 when sockets became endpoints and 2026-11-04 when the segment
  became `.socket` with the protocol in the session name: an `Endpoint` wraps
  a [socket](/doc/concept/socket.md) endpoint, which since 2026-11-06 is a
  server at `.server/<id>`, a client at `.client/<id>`, or both.
  `connect` dials with the caller's own endpoint ID, and `accept` reads the
  remote ID from the dial, which the socket's answer check proves and the
  grant pins (publish `.server/<id>` and `.client/<id>` only).
- Streams are [byte streams](/quest/m3/stream/rust.md) at the protection
  level the service declares: identity reads `moq-net` directly, and
  authentication, the mesh default, reads through `moq_crypto::sign` against the
  `PublicKey` parsed from the peer's endpoint id, which names the endpoint
  broadcast it subscribes to.
- The secret key comes from `moq_crypto::sign::SecretKey::load_or_generate`.
- A mesh daemon (Kixel's, outside this repository, decided 2026-11-02) is
  built on this API, so a process either embeds an `Endpoint` or talks to a
  local [mesh relay node](/quest/m3/mesh/node.md).
- Write a short porting note into the [guide](/quest/m3/mesh/guide.md).

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): one socket endpoint per service, with a grant to publish on it under the member's own id, so `moq_net::socket` ships unchanged and needs no service field.

✅ Decided 2026-11-07 (maintainer, /quest-complete): scope both
namespaces. Members own `mesh/node/<id>/**` (was `mesh/<id>/**`) and each
service lives at `mesh/service/<service>` (was `mesh/<service>`), so the
grant adds publish `mesh/service/*/.server/<id>` and
`mesh/service/*/.client/<id>` (rejected: `mesh/.service/<service>`;
keeping `mesh/<service>` and refusing service names shaped like an endpoint
ID). Review of [moq-dev/moq#8214](https://github.com/moq-dev/moq/pull/8214)
found that each member publishing `mesh/<id>/**` lets a service named after
member A put `.server/B` under A's own grant, so A could answer as B; the
`node` and `service` segments keep the two apart. A service name is still
one path segment, validated when the `Endpoint` is built, so `*` in the
grant covers it (`video/h264` would fall outside).

Member grants change shape, which breaks every enrolled device's paths:

- `moq_auth::mesh`'s coordinator issues publish `mesh/node/<id>/**` and the
  two service patterns (`rs/moq-auth/src/mesh/coordinator.rs`), with its
  docs (`rs/moq-auth/src/mesh/mod.rs`), contract tests, and a `MIGRATE.md`
  entry replacing the `Pattern::subtree(&format!("mesh/{endpoint}"))` note.
- `doc/bin/relay/auth.md`'s mesh enrollment section names the new grant.
- moq-crypto's and `@moq/crypto`'s sign examples and tests that use
  `mesh/<key>/agent` paths move to `mesh/node/<key>/agent`, so the examples
  match the grant.
- A hosted coordinator implementing the JSON contract issues the same
  grant; the PR's Impact section says so.
- Rollout: an old client with a new grant, or a new client with an old
  grant, can't publish. Open, for whoever starts this, with the maintainer:
  recommended, the coordinator issues both the old and new patterns for one
  release while clients upgrade and refresh their tokens, then drops the old
  one, with the MIGRATE entry giving that order; the old pattern keeps the
  service-name collision open until it is dropped.

## Required

- [Rust byte streams](/quest/m3/stream/rust.md) - the stream this API returns
