# [S] Plan: what an auth re-check may update in place

## Goal

A settled contract for which fields a revalidation reply can change on a live
session and which force a reconnect. Scope narrowing is settled (close, and
the client reconnects into the narrower grant); `tier` and `alias` are not.
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

Decide deliberately, per field: update in place, close, or refuse the change.
Then the implementing quest applies it in `rs/moq-relay/src/auth.rs` with a
test per field and documents the contract in `doc/bin/relay/auth.md`.

## Related

- [#3058](https://github.com/moq-dev/moq/issues/3058) - the issue the implementing quest closes
- [Auth verdict](/quest/m2/auth-verdict.md) - the proxy mode whose re-check this also governs
