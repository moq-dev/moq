# [L] Dynamic range requests

## Goal

A publisher's `track::Dynamic` receives range requests for misses instead of
one request per group, and a track serves a subscription's ranges from one
cursor, in its order, without duplicating a group, naming every sequence in
range it won't deliver. `requested_group` and `fetch_group` are replaced, not
kept beside it.

## Plan

A subscriber already reads only its own ranges, re-resolved when they are
replaced. What remains is the miss: one below the cache becomes a range request
on `Dynamic` rather than a group request. Time anchors land separately; see
[time anchors](/quest/m1/subscribe-ranges/time-anchors.md).

- Replace `requested_group` and `fetch_group` rather than keep both. Update
  every caller (relay, moq-archive, hls, the ladder) in the same PR,
  including the archive consumer's time resolution from its key listing if it
  has landed, plus the
  published surfaces built on them, per the cross-package table: moq-ffi
  (`fetch_group`, `requested_group`), moq-c (`poll_requested_group`), every
  binding wrapper and its `doc/lib` page, and `moq fetch` through
  `moq_relay::fetch_group` (added in the 2026-10-05 audit).
- Every sequence in range either arrives or is named as dropped, so a reader
  never waits on a miss. Share the accounting with
  SUBSCRIBE_DROP rather than adding a second one:
  the cursor's drop scan already names only sequences inside the reader's own
  ranges.
- Decided in the 2026-10-10 audit: `max_delay` caps every range, including
  historical FETCH replacements, and publisher `max_age` always clamps it.
  A FETCH with no reader delay uses publisher max-age alone, not the live
  SUBSCRIBE zero-delay default. Preserve that distinction when translating
  FETCH into a range request. Sharing a range never relaxes a caller's
  budget or lets one expired caller cancel another still-valid reader.
  Reuse FETCH's optional per-reader budget and publisher clamp rather than
  restoring a historical exemption.
- Benchmark filling over range count and span as separate axes, beside
  `track_range_serving`, which covers serving.

Open questions, each with a recommendation (found starting the quest,
2026-10-20; settle before implementing):

1. **Handler shape.** Recommended: `Dynamic::requested_fill` yields a
   `track::Fill` naming one bounded run (`start: Position`, exclusive end
   group, priority, the most tolerant budget, order). The handler calls
   `accept(sequence, info)` per group it has and `drop_groups(range)` for
   the rest, and dropping the `Fill` names whatever is left as dropped, into
   the track's existing `drops`. Rejected: writing through
   `Producer::create_group`, since a fetch-only copy or an archive track has
   no live producer and backfill must stay out of the arrival walk.
2. **What a miss is.** Recommended: a sequence in a reader's window, below its
   cap, that the cache doesn't cover and the feed will never deliver: below
   the feed's start, or at or below its newest group and not awaited by the
   arrival ledger (evicted or skipped). Never above that frontier, which
   arrives live, so a fill is bounded by construction. A relay records the
   span a SUBSCRIBE_START skipped past its requested floor as dropped (today
   it only settles the tail), or it would FETCH what upstream already gave up.
3. **Fetch budget.** Recommended: `Subscription::max_delay = Duration::MAX`
   means publisher max-age alone, and the wire saturates it at the varint
   maximum (today `Duration::MAX` fails to encode). Rejected:
   `Option<Duration>`, which breaks `Subscription` in every binding for no
   behavioral gain.
4. **Ending a bounded read.** A one-range subscription replaces `fetch_group`
   only if `recv_group` returns `None` once every sequence in a bounded window
   is delivered, dropped, stale, or unfillable. Today it parks past the
   window's end on purpose, so an update can still widen it. Recommended: end
   it, since a reader wanting more keeps an open range, and let a miss surface
   as that `None`
   (moq-ffi maps it to `NotFound`), giving up the `NotFetchable` and `Old`
   distinctions no caller acts on.
5. **Sessions in this PR.** Recommended: a session serves a fill as it serves
   a group request today, one FETCH per group, one in flight per fill, newest
   first for a descending reader, stopping when demand leaves and dropping the
   rest once a fetched group is stale under the fill's budget (a timed
   track's starts never rewind). [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md)
   turns lite-07 fills into range subscriptions, and
   [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) into one FETCH
   per run. Until then one SUBSCRIBE range can cost up to frontier minus floor
   serialized upstream FETCHes.
6. **Split.** Recommended: two PRs, each breaking one API once. First,
   `requested_group` becomes `requested_fill` everywhere (sessions, archive,
   the ladder, moq-ffi and bindings) and subscriptions fill their misses,
   with `fetch_group` misses queueing one-group fills meanwhile. Second,
   `fetch_group` goes for a one-range subscription in every caller, binding,
   and `moq fetch`, with the fill benchmark. The second needs head fills (a
   cached live group missing the frames a range starts at), so it requires
   [frame bounds](/quest/m1/subscribe-ranges/frame-bounds.md), which is
   reworking the same cursor now.
7. **moq-c.** Recommended: if the generated C package lands first ([moq-dev/moq#6052](https://github.com/moq-dev/moq/pull/6052),
   and [moq-dev/moq#6092](https://github.com/moq-dev/moq/pull/6092) proposes it as a blocker), `poll_requested_group` leaves the
   hand-written ABI with a `MIGRATE.md` entry and the fill reaches C through
   the generated API. Otherwise the frozen crate splits each fill into its
   per-group requests internally, keeping its ABI unchanged.
