# [L] Consumers see the producer's real error, never Dropped

## Goal

A track that ends because its source ended reports the source's own error to
every consumer, locally and across a relay. `Dropped` means only that a handle
was dropped without an end, which a correct producer never does. A broadcast
end carries no cause since #4031, so only track errors are in scope.
Consumers never decide behavior by matching an error; errors are diagnostics.

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

- Decided 2026-10-12 from [#5347](https://github.com/moq-dev/moq/issues/5347):
  `moq export ts --linger --stitch` exited on `internal error` when its
  catalog TRACK request landed on a publisher session that was dead but not
  yet timed out. On epochless lite-06 the front is offered only its first
  route, so when that session timed out, `Front::selected(None)` ended the
  resolved front with `Dropped`, `TrackIo::end` rejected the request with it,
  and it went out as Internal. A `Dropped` that reaches the wire means some
  path never ended with a proper error; diagnose each one at its root rather
  than remapping it.
  - A route that fails under a request or track (the upstream session dies or
    times out, and nothing replaces the copy) ends downstream with
    `Unroutable`, for pending requests and for tracks whose copy ends with the
    upstream's `Transport` or `Session` error. The downstream session is
    healthy; only the path lost its route. This matches
    `resume::route_failed`, which already counts those as a route failure.
  - Give every variant the catch-alls swallow a real arm, `Transport`
    included (also `TooManyRequests`, `Unsupported`, `Duplicate`,
    `InvalidRange`, `FrameOpen`, `Encode`, `InvalidJoiningRequestId`).
  - Remote errors stay nested (`Error::Stream(..)`), as `Error::stream()`
    promises; nothing depends on flat values once the follower stops
    matching. Flattening was considered and dropped on #5358's review.
  - `ts::Follower` stops matching errors. While lingering, announcements
    decide whether the broadcast returned: any resolve error returns it to
    waiting for the next announcement, and a new announcement or end cancels
    an in-flight resolve instead of sitting out a dead session's idle
    timeout. `--linger` is the only exit; at expiry it fails with the last
    resolve error if there was one, otherwise with the original end.
  - Regressions: a relay test where a TRACK request is in flight to an
    upstream session that then times out, and the downstream reset is
    Unroutable; a follower test where a resolve error while lingering keeps
    lingering until the instance returns, and a refusal surfaces at expiry.

Public API: none expected; error values consumers observe change. Wire: none.

## Required

- [Auth](/quest/m1/auth/README.md) - its Unauthorized quest (#4179, done on the line) supplies shared Unauthorized errors and revoked-stream handling

## Closes

- [#5347](https://github.com/moq-dev/moq/issues/5347) - `export ts --stitch --linger` exits on `internal error` when a catalog request lands on a dying publisher

## Related

- [#4179](https://github.com/moq-dev/moq/pull/4179) - owns the revoked-upstream path
