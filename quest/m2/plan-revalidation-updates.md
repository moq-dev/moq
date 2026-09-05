# [S] Plan: what an auth re-check may update in place

## Goal

A settled contract for which fields a revalidation reply can change on a live
session and which force a reconnect. Scope narrowing is settled by the
[relay auth](/quest/m2/path-patterns/relay-auth.md) contract: the session
stays up, resized to the narrower grant, and only the subscriptions and
publications the grant no longer covers are closed; `tier` and `alias` are
not settled.
Run `/plan-quest`; the settled plan becomes the implementing quest that closes
the issue.

## Plan

`Auth::recheck` scores a reply by `Scope::covered_by` (root, subscribe,
publish), then drops the `AuthToken` and propagates only the `CacheHints`. Two
fields the auth API can legitimately change therefore behave inconsistently:

- `tier` is silently ignored. An endpoint that re-buckets a connection onto or
  off a billing tier has no effect until the session reconnects, and the tier
  decides which meter pays. Applying it mid-session means rebuilding the
  session's stats handle, and usage already recorded under the old tier stays
  there.
- `alias` closes the session, because the alias becomes the token's `root` and
  `covered_by` fails. Arguably correct, since the broadcast would otherwise keep
  announcing under a root the API no longer assigns, but it is a silent hard
  disconnect for what may be a benign rename.

Two things are fixed before this decision is made. The
[relay auth](/quest/m2/path-patterns/relay-auth.md) quest already requires a
re-check to compare the full versioned grant, reject unsupported, mixed,
invalid, and out-of-scope grants, and resize a live session with no prefix-only
widening window; this plan inherits that invariant rather than restating a
weaker one. And "the alias becomes the token's `root`" describes today's v0
behavior only: under v1 grants a literal root aliases or rebases the patterns
and never becomes one, so the canonical alias transformation has to be stated
before deciding whether an alias change closes or updates a session.

Then decide deliberately, per field: update in place, close, or refuse the
change.
Then the implementing quest applies it in `rs/moq-relay/src/auth.rs` with a
test per field and documents the contract in `doc/bin/relay/auth.md`.

## Related

- [#3058](https://github.com/moq-dev/moq/issues/3058) - the issue the implementing quest closes
- [Auth verdict](/quest/m2/auth-verdict.md) - the proxy mode whose re-check this also governs
