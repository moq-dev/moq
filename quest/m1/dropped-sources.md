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

- Decided 2026-10-11 from [#4349](https://github.com/moq-dev/moq/issues/4349),
  where a clean publisher close ended an in-flight subscription with
  `internal error` and subscribes during cross-node churn answered the same.
  `Dropped` has no arm in `From<&Error> for StreamError`, so it falls through
  the `_ => Internal` catch-all to 0x0.
  - Fronts end with `Error::Dropped` today (`model/front.rs`: `Retired`,
    `Closed`, a resolved front whose route leaves the table, `source_closed`).
    Requests and tracks a front actually aborts end with `Unroutable` instead,
    or `Cancel` on a local origin `Retired` or `Closed`.
  - Tracks in flight are not aborted: `Action::End` in `model/origin.rs`
    leaves them on their copy, which drains (ANNOUNCE_END does not disturb
    in-flight subscriptions) and ends with the source's own error. Trace that
    copy's end for the reported case rather than remapping the front.
  - Remove the `_ => Internal` catch-alls in both `From<&Error>` conversions
    (`StreamError` and `SessionError`); `Dropped` stays `Internal`
    deliberately, since it then marks a real bug.
  - Regression: a relay test closes a broadcast under an active subscription
    whose source copy ends with a specific non-clean error, and expects that
    exact error, never `internal error`; a subscribe against a withdrawn
    route expects `Unroutable`.

Public API: none expected; error values consumers observe change. Wire: none.

## Required

- [Auth](/quest/m1/auth/README.md) - its Unauthorized quest (#4179, done on the line) supplies shared Unauthorized errors and revoked-stream handling

## Related

- [#4179](https://github.com/moq-dev/moq/pull/4179) - owns the revoked-upstream path
