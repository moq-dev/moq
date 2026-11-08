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
  `mesh/<id>/`.
- Decided 2026-11-01 (maintainer, socket planning interview), and updated
  2026-11-03 when sockets became endpoints and 2026-11-04 when the segment
  became `.socket` with the protocol in the session name: an `Endpoint` wraps
  a [socket](/doc/concept/socket.md) endpoint, which since 2026-11-06 is a
  server at `.server/<id>`, a client at `.client/<id>`, or both.
  `connect` dials with the caller's own endpoint ID, and `accept` reads the
  remote ID from the dial, which the socket's answer check proves and the
  grant pins (publish `.server/<id>` and `.client/<id>` only). Update `moq auth`'s mesh grant and
  `doc/bin/relay/auth.md` to match.
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

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): one socket endpoint per service at prefix `mesh/<service>`, with the grant adding publish `mesh/*/.server/<id>` and `mesh/*/.client/<id>`, so `moq_net::socket` ships unchanged and needs no service field.

Two constraints on that layout, found in review of
[moq-dev/moq#8214](https://github.com/moq-dev/moq/pull/8214):

- A service name is one path segment, validated when the `Endpoint` is
  built, so `*` in the grant covers it (`video/h264` would fall outside).
- Service sockets must not overlap member subtrees. Each member may publish
  `mesh/<id>/**`, so a service named after member A puts `.server/B` under
  A's own grant, and A could answer as B. The prefix in the Waiting item
  below keeps the two apart.

## Waiting

- Maintainer: pick the service prefix that keeps service sockets out of
  `mesh/<id>/**`. Options: **`mesh/.service/<service>` (recommended)**,
  since endpoint IDs never start with `.`, so no member subtree can contain
  it, and the grant becomes publish `mesh/.service/*/.server/<id>` and
  `.client/<id>`; or keep `mesh/<service>` and refuse service names that
  parse as an endpoint ID, which leaves the check in every relay's grant
  logic. Done when the Plan names the prefix.

## Required

- [Rust byte streams](/quest/m3/stream/rust.md) - the stream this API returns
