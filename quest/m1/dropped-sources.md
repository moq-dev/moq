# [M] Consumers see the producer's real error, never Dropped

## Goal

A track that ends because its source ended reports the source's own error to
every consumer, locally and across a relay. `Dropped` means only that a handle
was dropped without an end, which a correct producer never does. A broadcast
end carries no cause ([Broadcast close](/quest/m1/broadcast-close/README.md)),
so only track errors are in scope.

## Plan

- Rust already maps IETF `PUBLISH_DONE` Unauthorized to `Error::Unauthorized`,
  and a closed source's standing route no longer re-requests it.
- #4179 fixes revoked upstream subscriptions and #4120 preserves session death
  and resumed-track errors. After #4179 reaches `main`, verify the remaining
  track paths (source close, route removal, broadcast withdrawal) locally and
  over a mock session, and map JS `PUBLISH_DONE` Unauthorized to #4179's shared
  error. Preserve causes at the source rather than remapping `Dropped` at
  consumers.

Public API: none expected; error values consumers observe change. Wire: none.

## Required

- [Unauthorized](/quest/m1/auth/unauthorized.md) - #4179 supplies shared Unauthorized errors and revoked-stream handling

## Related

- [#4179](https://github.com/moq-dev/moq/pull/4179) - owns the revoked-upstream path
