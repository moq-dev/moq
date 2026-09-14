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
  the admitted grant's `expires`. A non-empty token becomes a `connect`
  request through the session's `moq_auth::Client`, carrying the admitted
  session's path and transport with the token as its query and no certificate
  facts, since a certificate cannot be presented in band. mTLS sessions reject
  a non-empty token as `Unsupported`.
- Union: the connection holds the set of accepted leases. Every one
  must carry the admitted root, else `AUTH_ERROR { Unauthorized }` naming the
  root. The session's origin handles are rebuilt through `Cluster::publisher`
  and `Cluster::subscriber` from the union of the set's publish and subscribe
  prefixes, intersected with the role the client declared at SETUP so a
  publish-only session never starts receiving announcements because a later
  token happened to carry subscribe prefixes, and swapped in whenever the set
  grows, the first live re-scope the relay performs; keep it behind one
  function that
  [Origin scopes](/quest/m2/path-patterns/origin.md) later extends to a
  shrinking union.
- Expiry: the deadline today is the admitted lease's `closed()` inside one
  `tokio::select!` arm. Select on one lease per token in the set instead, and on any firing recompute the union without it: unchanged
  means `AUTH_ERROR { Expired }` on that token's stream and the session
  continues, shrunk means `session.abort(Unauthorized)` as today. A token
  withdrawn by the client (its stream closed) follows the same rule. Each
  lease revalidates on its own cadence, and a re-check that lowers a token's
  `expires` writes an update AUTH_OK on its stream so the client can present
  a replacement in time.
- Refusals from the auth server map as connect-time ones do: a `403` or an
  empty grant is `AUTH_ERROR { Unauthorized }`; an outage on a new token is
  `AUTH_ERROR { Timeout }`, and an outage during re-check keeps the token
  until its `expires`, never a close on its own.
- The client side names dev's API: `moq_tokio::Connection` gains `auth()`
  returning a handle the connection owns, not the current session's. It
  stores every token added through it, presents them on each new session as
  it attaches, unions the live session's grant, and its `add` resolves against
  the session that is up at the time; a token the app drops is withdrawn from
  the live session and forgotten. Branch from dev, or from main once
  [merge-dev](/quest/m1/merge-dev.md) lands.
- Docs: `doc/bin/relay/auth.md` gains an "in-band tokens" section beside
  revalidation stating that grants union, that a token needs the admitted
  root, what an expiry does to the union today, and that the grant's expiry
  is the token's `exp`.
- Tests: a second token with a later `exp` and the same scope keeps the
  session past the first token's expiry, with only the first stream ending;
  an expired or invalid token is refused and nothing changes; a token with a
  different root is refused naming it; a token adding a prefix rebuilds the
  origin scope and the prefix is now solicited; withdrawing the only token
  covering a prefix closes the session; an mTLS session refuses a token; a
  proxy-mode outage on re-check keeps the token.

Additive.

## Required

- [Lite stream](/quest/m2/auth/lite.md) - supplies the AUTH stream and
  `auth::Request` this consumes

## Related

- [Origin scopes](/quest/m2/path-patterns/origin.md) - extends the re-scope
  to a shrinking union
- [Relay](/quest/m1/auth/relay.md) - the tier and root outcomes a re-check
  has; a token that changes them follows the same rules
