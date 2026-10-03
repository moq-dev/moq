# [L] SUBSCRIBE_OK carries the live media time

## Goal

When a route answers a lite-07 subscription, the subscriber learns the
publisher's current media time, extrapolated to now rather than the newest
frame's timestamp. A front then reveals an idle track's hidden cache as soon as
the next route answers, and every reader's budget judges that cache against the
real live edge, even across a gap.

Today (since [#4741](https://github.com/moq-dev/moq/pull/4741)) the serving
route's live edge settles a hidden cache: moq-transport's Largest Location from
its answer, or the newest group a lite route delivers (a lite answer carries no
edge). An edge within the cache brings it back; one past it leaves the cache
fetch-only, because nothing bounds how old it is, even when a reader's budget
would still accept a cached group. The route's resolved start was tried as the
signal and dropped: lite-05 answers before its SUBSCRIBE_START, and a shared
copy reports an older subscription's start. A sparse track that delivers
nothing for a while must still be judged on the answer, never on a frame.

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
  without negotiation. Older versions keep the edge rule.
- **API: `track::Subscriber::live().await`** resolves once the route answers the
  subscription, to `Live { start, latest: Option<Position>, time: Option<Timestamp> }`:
  the resolved start, the newest group/frame, and the live media time. Mirrored in
  js/net. It grows out of the crate-private `track::Request::with_edge` and
  `track::Consumer::edge` that carry moq-transport's Largest Location today, and the
  front's pump switches to it from those and `track::Consumer::poll_start`.
- **The front's unpark** reveals the whole hidden cache when the answer carries
  a live time, and readers' budgets measure staleness against it instead of the
  cache's frozen edge. Without one (untimed, older version), the edge rule
  stays.
- **Independent of [Subscribe ranges](/quest/m1/subscribe-ranges/README.md)**,
  which rewrites the same messages: whichever lands second rebases.
- `js/watch/src/sync.ts` stays as it is: its latency range (from #1620) is
  intended, and only the publisher's estimate uses the least-delayed reference.
- No new docs page: the lite draft and the max-age paragraph in
  `doc/concept/moq-lite.md` are updated inline.

## Related

- [Untimed model](/quest/m1/untimed-model.md) - absent timestamps end to end; only real pairs feed the estimate
- [Shared clock](/quest/m2/shared-clock.md) - the hang catalog's `{wall, timescale}` anchor sits a layer up; this stays media-agnostic
