# [L] SUBSCRIBE_OK carries the live media time

## Goal

When a route answers a lite-07 subscription, the subscriber learns the
publisher's current media time, extrapolated to now rather than the newest
frame's timestamp, so every reader's budget judges a cache against the real
live edge, even across a gap.

Today (since [#4741](https://github.com/moq-dev/moq/pull/4741)) a front caches
nothing: its readers read the serving route's copy directly, and a track nobody
reads drops that copy, so an idle cache is never served. lite-07 SUBSCRIBE_OK
carries the largest position from the publisher's cache, which nothing reads yet.
A budget judges a group by where its successor starts, so past a gap an old
group's reach stays open. Re-scope before starting: the idle-cache case that
motivated this is gone.

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
  is absent, as its frame timestamps are. An untimed track never reports one,
  and no receiver invents one.
- **lite-07 only.** It is still WIP (`moq-lite-07-wip`), so the field is added
  without negotiation. Older versions keep the gap rule.
- **API: `track::Subscriber::live().await`** resolves once the route answered, to
  `Live { start, latest: Option<Position>, time: Option<Timestamp> }`: the resolved
  start, the largest group/frame, and the live media time. Mirrored in js/net.
- **Independent of [Subscribe ranges](/quest/m1/subscribe-ranges/README.md)**,
  which rewrites the same messages: whichever lands second rebases.
- `js/watch/src/sync.ts` stays as it is: its latency range (from #1620) is
  intended, and only the publisher's estimate uses the least-delayed reference.
- No new docs page: the lite draft and the max-age paragraph in
  `doc/concept/moq-lite.md` are updated inline.

## Related

- [Untimed model](/quest/m1/untimed-model.md) - absent timestamps end to end; only real pairs feed the estimate
- [Shared clock](/quest/m1/shared-clock.md) - the hang catalog's `{wall, timescale}` anchor sits a layer up; this stays media-agnostic
