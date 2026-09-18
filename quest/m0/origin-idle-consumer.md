# [M] An idle track releases its source at the unused edge

## Goal

A `moq-net` origin front stops consuming the source's track the moment its
last reader leaves, and keeps only its own producer, with the groups it
cached, warm for `TRACK_IDLE_LINGER`. Today `serve_track` in
`rs/moq-net/src/model/origin.rs` holds the source `track::Consumer` for the
whole 30 s window, so every producer behind an origin counts the front as a
reader for 30 s after the real one left: a relay keeps its upstream track
subscribed on every hop, and `used`/`unused` on libmoq, moq-ffi, and every
binding reports `unused` 30 s late. The doc comment on `TRACK_IDLE_LINGER`
already says a warm copy holds no upstream subscription; the code disagrees on
main and dev alike.

## Plan

Branch from dev: the origin front was rewritten there and the merge carries
the fix to main. Before the change, reproduce it as a test: consume a track
through an origin, drop the consumer, and assert the source producer's
`unused()` resolves within a bound far below the linger. Then:

- On the `Step::Demand` edge to unused, drop `serving` (the source copy) at
  once and keep the deadline. `resume`, the front's own producer, stays
  spliced with the segment it delivered so a returning reader within the
  window reads cached groups without a new request or a second `TRACK_INFO`
  round trip. `Step::Idle` still releases the segment when the window ends.
- A reader returning inside the window re-splices from the best route as a
  fresh `Splice`; only groups past the cached edge cost an upstream request.
  The subscriber side (`lite/subscriber.rs` and `ietf/subscriber.rs`) already
  cancels upstream on its producer's unused edge and needs no change; fix
  its comment that says the origin releases the copy after the linger.
- Confirm the HLS cadence still holds: a `moq-hls` playlist polled every
  target duration is answered from the cache between polls, and the existing
  HLS tests plus the merge-dev soak prove a days-old broadcast still serves a
  fresh viewer promptly.
- Fix `rs/moq-relay/tests/drills.rs` and the decode consumer comments that
  describe the old 30 s upstream hold.

Public API: none. Wire: none. Behavior: upstream subscriptions end when the
last reader leaves instead of 30 s later.

## Related

- [Track demand](/quest/m1/libmoq-track-demand.md) - the C ABI watchers that first measured the 30 s delay
