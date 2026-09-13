# [M] Relay tokens

## Goal

A client presents tokens in band and moq-relay verifies each through the same
`Auth` path the URL token took: the session's scope is the union of every
accepted token, a refused token gets AUTH_ERROR and changes nothing, and a
session whose credential is about to lapse renews by presenting a fresh one
before the old expires. The grant a client receives carries the token's real
expiry, scope loss cancels only the work it no longer authorizes, and
`doc/bin/relay/auth.md` documents the exchange.

## Plan

- `Connection::run` in `rs/moq-relay/src/connection.rs` takes `requests()`
  from the `moq_net::Request` builder before `.ok()`, so the relay owns the
  initial empty AUTH too and the driver's fallback never races it. An empty
  token is answered from the origin handles as the default does, plus
  `token.expires` from the admitted `AuthToken`. A non-empty token goes
  through `Auth::verify` with `AuthParams { path, jwt, transport }` built from
  the admitted session's path and transport; `verify_mtls` never, since a
  certificate cannot be presented in band. mTLS sessions reject a non-empty
  token as `Unsupported`.
- Union: the connection holds the set of accepted `AuthToken`s. Every one
  must carry the admitted root, else `AUTH_ERROR { Unauthorized }` naming the
  root. The session's origin handles are rebuilt through `Cluster::publisher`
  and `Cluster::subscriber` from the union of the set's publish and subscribe
  scopes, intersected with the role the client declared at SETUP so a
  publish-only session never starts receiving announcements because a later
  token happened to carry subscribe scope. Use the resize operation supplied
  by [Relay auth](/quest/m2/path-patterns/relay-auth.md) for both growth and
  shrinkage; do not implement a second scope-swap path here.
- Expiry: the deadline today is a future borrowing the admitted token inside
  one `tokio::select!` arm (`Auth::expired`). Run one `expired` per token in
  the set instead, and on any firing recompute the union without it: unchanged
  means `AUTH_ERROR { Expired }` on that token's stream and the session
  continues. A shrinking union uses the common resize to cancel only the
  publications and subscriptions that lose authorization, including active
  work, while the session and still-authorized work continue. An empty union
  grants nothing but keeps the connection available for a fresh token. A token
  withdrawn by the client follows the same resize rule. The
  revalidation cadence and staleness window run per token as well, and a
  proxy-mode re-check that lowers a token's `exp` writes an update AUTH_OK on
  its stream so the client can present a replacement in time.
- Refusals from the auth API in proxy mode map as connect-time ones do: `404`,
  empty grant, `401`, `403` are `AUTH_ERROR { Unauthorized }`; an outage on a
  new token is `AUTH_ERROR { Timeout }`, and an outage during re-check keeps
  the token as the staleness rule already does, never a close on its own.
- The client side names dev's API: `moq_tokio::Connection` gains `auth()`
  returning a handle the connection owns, not the current session's. It
  stores every token added through it, presents them on each new session as
  it attaches, unions the live session's grant, and its `add` resolves against
  the session that is up at the time; a token the app drops is withdrawn from
  the live session and forgotten. Branch from main once
  [merge-dev](/quest/m1/merge-dev.md) lands.
- Docs: `doc/bin/relay/auth.md` gains an "in-band tokens" section beside
  revalidation stating that grants union, that a token needs the admitted
  root, how expiry resizes the union without disconnecting, and that the grant's expiry
  is the token's `exp`.
- Tests: a second token with a later `exp` and the same scope keeps the
  session past the first token's expiry, with only the first stream ending;
  an expired or invalid token is refused and nothing changes; a token with a
  different root is refused naming it; a token adding a prefix rebuilds the
  origin scope and the prefix is now solicited; withdrawing the only token
  covering a path cancels only its affected active work; overlapping grants
  preserve work still covered; removing the final token leaves a connected
  empty-scope session that can accept a fresh token; a grant update that
  narrows scope follows the same rule; an mTLS session refuses a token; a
  proxy-mode outage on re-check keeps the token.

Additive.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - release the origin scope and connection surfaces used by AUTH

- [Pattern interest](/quest/m2/path-patterns/interest.md) - AUTH can represent the complete grants relay revalidation returns

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - supplies the common live scope resize before token expiry uses it

- [Lite stream](/quest/m2/auth/lite.md) - supplies the AUTH stream and
  `auth::Request` this consumes

## Related

- [Revalidation updates](/quest/m1/auth-api/revalidation-updates.md) - the tier and
  alias outcomes a re-check has; a token that changes them follows the same
  rules
