# [M] Relay refresh

## Goal

A client sends a new token in band and moq-relay verifies it through the same
`Auth` path the URL token took: the session's grant and expiry move to the new
token, a refused or narrowing token gets AUTH_ERROR and leaves the session on
its current grant, and a session whose credential is about to lapse can renew
without reconnecting. The grant a client receives carries the token's real
expiry, and `doc/bin/relay/auth.md` documents the exchange.

## Plan

- `Connection::run` in `rs/moq-relay/src/connection.rs` consumes
  `session.auth().requests()`. An empty token is answered from the origin
  handles as the default does, plus `token.expires` from the admitted
  `AuthToken`. A non-empty token goes through `Auth::verify` with
  `AuthParams { path, jwt, transport }` built from the admitted session's path
  and transport, or `verify_mtls` never, since a certificate cannot be
  replaced in band. mTLS sessions reject a non-empty token as `Unsupported`.
- Coverage: the new token's root must equal the admitted root, and its publish
  and subscribe prefixes must cover the current grant (`Scope::covered_by`, the
  check `Auth::recheck` already performs). Anything else is `AUTH_ERROR
  { Unauthorized, reason }` naming which side failed, and nothing changes.
  Widening is accepted: the session's origin handles are rebuilt through
  `Cluster::publisher` and `Cluster::subscriber` from the new token and
  swapped in, which is the first live re-scope the relay performs, so keep it
  behind one function that [Relay auth](/quest/m2/path-patterns/relay-auth.md)
  later extends to narrowing.
- Expiry: the deadline today is a future borrowing the admitted token inside
  one `tokio::select!` arm (`Auth::expired`). Hold the current token in a cell
  the refresh path replaces and re-enter `expired` on change, so the JWT
  `exp`, the revalidation cadence, and the staleness window all follow the new
  token. When revalidation lowers `exp` on a proxy-mode session, write an
  unprompted AUTH_OK with the new expiry so the client can refresh in time.
- Refusals from the auth API in proxy mode map as connect-time ones do: `404`,
  empty grant, `401`, `403` are `AUTH_ERROR { Unauthorized }`; an outage keeps
  the session on its current grant and is reported as `AUTH_ERROR
  { Timeout }`, never a close.
- `moq_native::Reconnect` gains `auth()` returning the live session's handle
  (`None` while disconnected, the shape `ConnectionStatsReader` already uses),
  and remembers the last accepted token so the next reconnect presents it in
  the URL query until [Token in band](/quest/m2/auth/token-in-band.md)
  replaces that.
- `moq` CLI: `moq publish` and `moq serve` document that a token in the URL
  is what the initial grant reflects; no new flag until the credential leaves
  the URL.
- Docs: `doc/bin/relay/auth.md` gains an "in-band refresh" section beside
  revalidation stating what a refresh may change, that narrowing is refused
  today, and that the grant's expiry is the token's `exp`.
- Tests: a refresh with a later `exp` moves the deadline and the session
  outlives the original expiry; an expired or invalid token is refused and the
  session runs to the original deadline; a narrower or different-root token is
  refused with a reason naming the side; a widening token rebuilds the origin
  scope and a previously unsolicited prefix is now solicited; an mTLS session
  refuses a token; a proxy-mode outage keeps the grant.

On main, additive.

## Required

- [Lite stream](/quest/m2/auth/lite.md) - supplies the AUTH stream and
  `auth::Request` this consumes

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - extends the re-scope
  to narrowing
- [Revalidation updates](/quest/m2/revalidation-updates.md) - the tier and
  alias outcomes a re-check has; a refresh that changes them follows the same
  rules
