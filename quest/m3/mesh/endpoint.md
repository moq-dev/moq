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
- Streams are [byte streams](/quest/m1/stream/rust.md) at the protection
  level the service declares: identity reads `moq-net` directly, and
  authentication, the mesh default, reads through `moq_crypto::sign` against the
  `PublicKey` parsed from the peer's endpoint id, which names the endpoint
  broadcast it subscribes to.
- The secret key comes from `moq_crypto::sign::SecretKey::load_or_generate`.
- A mesh daemon (Kixel's, outside this repository, decided 2026-11-02) is
  built on this API, so a process either embeds an `Endpoint` or talks to a
  local [mesh relay node](/quest/m3/mesh/node.md).
- Write a short porting note into `doc/` beside the Endpoint's own page; the
  mesh [guide](/quest/m5/mesh-guide.md) is parked.

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

The grant change itself is its own quest, in m2, so it lands before the cut
publishes `moq_auth::mesh` (split 2026-11-12, maintainer, /quest-audit):
[mesh grants scope nodes and services apart](/quest/m2/mesh-grant-shape.md).
This quest builds the `Endpoint` on the new grant, and the dual-grant rollout
question it carried is gone, since no release ever issued the old shape.

## Required

- [Rust byte streams](/quest/m1/stream/rust.md) - the stream this API returns
- [Mesh grant shape](/quest/m2/mesh-grant-shape.md) - the grant that lets a member publish its service endpoints
