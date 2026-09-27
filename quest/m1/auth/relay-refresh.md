# [M] Relay tokens

## Goal

A client presents tokens in band and moq-relay verifies each through the same
`Auth` path the URL token took: the session's scope is the union of every
accepted token, a refused token gets AUTH_ERROR and changes nothing, and a
session whose credential is about to lapse renews by presenting a fresh one
before the old expires. The grant a client receives carries the token's real
expiry, an expiry that leaves the union intact ends only that token, and
`doc/bin/relay/auth.md` documents the exchange.

## Plan

- `Connection::run` in `rs/moq-relay/src/connection.rs` takes `requests()`
  from the `moq_net::Request` builder before `.ok()`, so the relay owns the
  initial empty AUTH too and the driver's fallback never races it. An empty
  token is answered from the origin handles as the default does, plus
  the admitted grant's `expires`. A non-empty token is presented through
  `Client::attach(&connection_lease, request)`, a method this quest adds:
  the request carries the connection's `id`, path, and transport, the query
  `jwt=<token>` exactly as the URL would have, and no certificate facts,
  since a certificate cannot be presented in band. An `attach` lease
  revalidates like any other but never POSTs `end`, and the server treats a
  `connect` for an id it already holds as one more grant on that session,
  so lifecycle stays with the connection lease and a withdrawn token frees
  no session-limit slot. `doc/bin/relay/auth.md` states that rule. mTLS
  sessions reject a non-empty token as `Unsupported`.
- Union: the connection holds the set of accepted leases. Every one
  must carry the admitted root, else `AUTH_ERROR { Unauthorized }` naming the
  root. The session's origin handles are rebuilt through `Cluster::publisher`
  and `Cluster::subscriber` from the union of the set's publish and subscribe
  prefixes, intersected with the role the client declared at SETUP so a
  publish-only session never starts receiving announcements because a later
  token happened to carry subscribe prefixes, and swapped in whenever the set
  grows; keep it behind one function, and a shrinking union goes through
  [Origin narrowing](/quest/m1/auth/narrowing.md) rather than a close.
- Expiry: the deadline today is the admitted lease's `closed()` inside one
  `tokio::select!` arm. Select on one lease per token in the set instead, and on any firing recompute the union without it: unchanged
  means `AUTH_ERROR { Expired }` on that token's stream and the session
  continues, shrunk narrows the origin and resets each stream that lost
  access with `UNAUTHORIZED`. A token
  withdrawn by the client (its stream closed or reset) recomputes the union
  the same way with nothing written back, since the stream is gone: unchanged
  means the session continues, shrunk narrows the same way. Each lease revalidates on
  its own cadence, and every re-check that changes the grant, a lower
  `expires` or a different `publish` or `subscribe`, writes the complete
  replacement grant as an update AUTH_OK on that token's stream and
  recomputes the union from the latest grant of every token, so the client
  can present a replacement in time and a narrowed token never keeps its old
  scope; a shrunk union narrows as above.
- Refusals map as connect-time ones do and never expose a status: every
  refusal `Client::attach` returns for a new token, a `403`, an empty or
  invalid grant, an unparseable body, or a `5xx`, is
  `AUTH_ERROR { Unauthorized }`, and a timeout is `AUTH_ERROR { Timeout }`.
  On re-check a refusal closes that token's stream with
  `AUTH_ERROR { Unauthorized }` and recomputes the union, while a failed
  re-check (timeout or `5xx`) keeps the token until its `expires`, never a
  close on its own.
- The client side: `moq_tokio::Connection` gains `auth()`
  returning a handle the connection owns, not the current session's. It
  stores every token added through it, presents them on each new session as
  it attaches, unions the live session's grant, and its `add` resolves against
  the session that is up at the time; a token the app drops is withdrawn from
  the live session and forgotten.
- Docs: `doc/bin/relay/auth.md` gains an "in-band tokens" section beside
  revalidation stating that grants union, that a token needs the admitted
  root, what an expiry does to the union today, and that the grant's expiry
  is the token's `exp`.
- Tests: a second token with a later `exp` and the same scope keeps the
  session past the first token's expiry, with only the first stream ending;
  an expired or invalid token is refused and nothing changes; a token with a
  different root is refused naming it; a token adding a prefix rebuilds the
  origin scope and the prefix is now solicited; withdrawing the only token
  covering a prefix resets that prefix's streams with `UNAUTHORIZED` and
  the session stays connected; an mTLS session refuses a token; a
  proxy-mode outage on re-check keeps the token.

Additive.

## Required

- [Origin narrowing](/quest/m1/auth/narrowing.md) - the live re-scope a shrinking token union needs, so no temporary close-on-shrink policy ships
- [Pattern interest](/quest/m1/path-patterns.md) - AUTH can represent the complete grants relay revalidation returns
- [Lite stream](/quest/m1/auth/lite.md) - supplies the AUTH stream and
  `auth::Request` this consumes
- [Unauthorized reset](/quest/m1/auth/unauthorized.md) - the code this
  relay's revocations reset with
