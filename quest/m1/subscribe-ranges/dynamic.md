# [L] Dynamic range requests

## Goal

A publisher's `track::Dynamic` receives range requests for misses instead of
one request per group, and every sequence in a subscription's range either
arrives or is named as dropped, so a reader never waits on a miss.
`requested_group` is replaced by `requested_fill`, not kept beside it, in every
caller and binding (the frozen moq-c ABI excepted, per the moq-c decision
below). Removing `fetch_group` is the follow-up
[fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md).

## Plan

A subscriber already reads only its own ranges, re-resolved when they are
replaced. What remains is the miss: one below the cache becomes a range request
on `Dynamic` rather than a group request. A time anchor the cache can't
resolve passes to the handler unresolved in its range request.

- Replace `requested_group` rather than keep both. Update every caller
  (sessions, relay, moq-archive, hls, the ladder) in the same PR, including
  the archive consumer's time resolution from its key listing if it has
  landed, plus the published surfaces built on it, per the cross-package
  table: moq-ffi (`requested_group`), every binding wrapper and its `doc/lib`
  page. Until the follow-up lands, a `fetch_group` miss queues a one-group
  fill.
- Every sequence in range either arrives or is named as dropped. Share the
  accounting with SUBSCRIBE_DROP rather than adding a second one: the cursor's
  drop scan already names only sequences inside the reader's own ranges.
- Decided in the 2026-10-10 audit: `max_delay` caps every range, including
  historical FETCH replacements, and publisher `max_age` always clamps it.
  A FETCH with no reader delay uses publisher max-age alone, not the live
  SUBSCRIBE zero-delay default. Sharing a range never relaxes a caller's
  budget or lets one expired caller cancel another still-valid reader.

Decided by the maintainer on 2026-10-20, when the quest was first started
([moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)):

- **Split.** This quest replaces `requested_group`; removing `fetch_group`
  is [fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md), so each
  PR breaks one API once.
- **Handler shape.** `Dynamic::requested_fill` yields a `track::Fill` naming
  one bounded run: start group, exclusive end group, priority, the most
  tolerant budget among its readers, and order. Each reader's cursor still
  skips what is stale under its own `max_delay`, so sharing never relaxes it.
  The handler calls `accept(sequence, info)` for each group it has and
  `drop_groups(range)` for the rest. Dropping the `Fill`
  names whatever is left as dropped, in the track's existing `drops`, but only
  while a reader still wants it: once demand leaves, the fill is withdrawn like
  a `group::Request` today and records nothing, so `drops` (durable) never
  hides still-fetchable history from a later reader.
  Rejected: writing through `Producer::create_group`, since a fetch-only copy
  or an archive track has no live producer, and backfill must stay out of the
  arrival walk.
- **What a miss is.** A sequence in a reader's window, below its cap, that
  the cache doesn't cover and the feed will never deliver: below the feed's
  start, or at or below its newest group and not awaited by the arrival
  ledger (evicted or skipped). Nothing above that frontier is a miss, since it
  arrives live, so a fill is bounded by construction. A relay records the
  span a SUBSCRIBE_START skipped past its requested floor as dropped (today it
  only settles the tail), or it would FETCH what upstream already gave up.
- **Fetch budget.** `Subscription::max_delay` becomes `Option<Duration>`:
  `None` means publisher max-age alone, the FETCH default above. This breaks
  `Subscription` in every binding, with a `MIGRATE.md` entry in each affected
  package. Rejected (recommended, but overruled): a `Duration::MAX` sentinel
  that the wire saturates, which avoids the break but encodes the meaning in a
  magic value.
- **Sessions.** A session serves a fill as it serves a group request today:
  one FETCH per group, one in flight per fill, newest first for a descending
  reader, withdrawing (not dropping) the fill when demand leaves. Once a
  fetched group is stale under the fill's budget, the groups older than it
  are too (a timed track's starts never rewind), so the fill stops fetching
  them; in either order, newer groups still arrive. They are withdrawn, not
  dropped, and each reader's cursor names them stale under its own budget,
  since a later reader with a wider one may still fetch them. Only staleness
  under publisher max-age, which binds every reader, goes in `drops`.
  [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) turns lite-07
  fills into range subscriptions, and
  [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) into one FETCH
  per run. Until then, one SUBSCRIBE range can cost up to frontier minus floor
  serialized upstream FETCHes, each bounded by the fill's budget and stopped
  when demand leaves, the same cost per-group requests carry today; accepted
  rather than blocking on those quests.
- **moq-c.** Whichever lands first decides it. If the generated C package
  ([moq-dev/moq#6052](https://github.com/moq-dev/moq/pull/6052)) has landed,
  `poll_requested_group` leaves the hand-written ABI with a `MIGRATE.md` entry
  and the fill reaches C through the generated API. Otherwise the frozen crate
  splits each fill into its per-group requests internally, keeping its ABI
  unchanged, and maps `moq_subscription.max_delay_us` to `Some`: a frozen C
  subscriber cannot ask for max-age alone, as today. Not a blocker.
- **Bench.** Fills fan out, so add a fill bench swept over both axes,
  subscribers x misses, so a cost that grows with either shows up as a slope
  (decided 2026-10-21).

## Related

- [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) - removes `fetch_group` once fills land
