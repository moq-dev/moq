# [L] SUBSCRIBE_OK carries the live media time

## Goal

When a route answers a lite-07 subscription, the subscriber learns the
publisher's current media time, extrapolated to now rather than the newest
frame's timestamp. A front then serves an idle track's cache as soon as
the next route answers, and every reader's budget judges that cache against the
real live edge, even across a gap.

Today (since [#4741](https://github.com/moq-dev/moq/pull/4741)) a track has a live
state: readers get nothing from a cache that is not live, and a relay's idle track
goes live again once its source says where its feed is, by its largest position
(lite-07 SUBSCRIBE_OK, moq-transport's Largest Location) or its first frame (older
lite). Within the cache, readers' budgets judge it; past a gap, the groups below
it stay fetch-only, because a missing successor leaves an old group's reach open.
A live media time in the answer would let the budget judge across a gap too.

## Plan

Decided in planning (2026-10-03):

- **Estimator: least-delayed reference.** Each track keeps
  `offset = min(wall − pts)` over a recent sliding window of the frames written
  to it (the least-delayed frame sets the reference, the window follows clock
  drift), and reports `now − offset` in the track's timescale. Reason: the
  newest frame's timestamp is stale on a sparse track and carries that frame's
  delivery jitter.
- **Lives in moq-net track state**, fed by every frame a `track::Producer`
  writes, so origin publishers and relays share one implementation and any
  SUBSCRIBE_OK reads it. js/net gets the same in its track state.
- **Each hop estimates on its own.** A relay reports its own estimate from the
  `(pts, arrival)` pairs it received, never the upstream's value, so the delay
  every hop adds shows up honestly.
- **Optional on the wire.** An untimed track (see
  [Untimed lite-07](/quest/m1/lite-untimed.md)) has no media time, so the field
  is absent, encoded the same way as an absent frame timestamp. A track whose
  frames are all untimed never reports one, and no receiver invents one.
- **lite-07 only.** It is still WIP (`moq-lite-07-wip`), so the field is added
  without negotiation. Older versions keep the gap rule.
- **API: `track::Subscriber::live().await`** resolves once the track is live, to
  `Live { start, latest: Option<Position>, time: Option<Timestamp> }`: the resolved
  start, the largest group/frame, and the live media time. Mirrored in js/net. It
  grows out of the crate-private `track::Producer::set_live` and
  `track::Subscriber::poll_live`.
- **Open: split `track::Producer` into live and fetch halves**, as a consumer splits
  into subscribing and fetching, instead of a live flag on one producer. Decide
  before the API goes public.
- **The front's unpark** serves the whole cache when the answer carries a live
  time, and readers' budgets measure staleness against it, gap or not. Without
  one (untimed, older version), the gap rule stays.
- **Independent of [Subscribe ranges](/quest/m1/subscribe-ranges/README.md)**,
  which rewrites the same messages: whichever lands second rebases.
- `js/watch/src/sync.ts` stays as it is: its latency range (from #1620) is
  intended, and only the publisher's estimate uses the least-delayed reference.
- No new docs page: the lite draft and the max-age paragraph in
  `doc/concept/moq-lite.md` are updated inline.

## Related

- [Untimed model](/quest/m1/untimed-model.md) - absent timestamps end to end; only real pairs feed the estimate
- [Shared clock](/quest/m2/shared-clock.md) - the hang catalog's `{wall, timescale}` anchor sits a layer up; this stays media-agnostic
