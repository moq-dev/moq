# [M] Consumers see the producer's real error, never Dropped

## Goal

A track that ends because its source ended reports the source's own error to
every consumer, locally and across a relay. `Dropped` means only that a handle
was dropped without an end, which a correct producer never does. A broadcast
end carries no cause since #4031, so only track errors are in scope.

## Plan

- Rust already maps IETF `PUBLISH_DONE` Unauthorized to `Error::Unauthorized`,
  and a closed source's standing route no longer re-requests it.
- #4179 fixes revoked upstream subscriptions and #4120 preserves session death
  and resumed-track errors. After #4179 reaches `main`, verify the remaining
  track paths (source close, route removal, broadcast withdrawal) locally and
  over a mock session, and map JS `PUBLISH_DONE` Unauthorized to #4179's shared
  error. Preserve causes at the source rather than remapping `Dropped` at
  consumers.
- The same goes for revocation. Leftovers from #4179's review: a bridged
  revocation reaches Rust IETF subscribers as `PUBLISH_DONE` InternalError, a
  JS relay reports a route revocation as INTERNAL_ERROR, and the bindings
  neither document nor test `is_auth` for a stream-scoped Unauthorized. Each
  should surface Unauthorized.

Public API: none expected; error values consumers observe change. Wire: none.

## Required

- [Auth](/quest/m1/auth/README.md) - its Unauthorized quest (#4179, done on the line) supplies shared Unauthorized errors and revoked-stream handling

## Related

- [#4179](https://github.com/moq-dev/moq/pull/4179) - owns the revoked-upstream path
