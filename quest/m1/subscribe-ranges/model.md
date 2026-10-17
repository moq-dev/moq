# [L] Model ranges

## Goal

The Rust model's `Subscription` carries a list of group ranges, aggregated
across subscribers. A track serves the union from one cursor without
duplicating a group, in the subscription's order, and reports every
sequence it won't deliver. A publisher's `track::Dynamic` receives range
requests for misses instead of one request per group.

## Plan

See the [line](/quest/m1/subscribe-ranges/README.md) for the decisions.
Guidance:

- Ranges have `Absolute`, `Relative`, or `Time` ends (see the line). A
  track resolves a `Time` end from its cached groups' timestamps and passes an
  unresolved one to its `track::Dynamic` handler; an untimed track refuses
  it. Test a time start and end against the cache, past it through a handler,
  and refused on an untimed track. Aggregating
  subscribers onto one upstream request is the union of their ranges; a
  relative end resolves against the track's latest group when the publisher
  serves it. Ranges are served in the existing `Subscription::order`, which
  merges with descending winning.
- This absorbs the deleted lite-07 Live flag quest, so the model mirrors the
  wire with no `live` field, and its tests move here, on mock time and both
  in-process and through a relay: a subscriber from `Absolute(4)` merged with
  a `Relative(0)..` subscriber on a quiet track whose newest group is 3 (the
  live one still gets group 3); a buffering `Relative(k)..` merged with a
  range above the live edge; a frame-precise start merged with live; an
  untimed `Absolute(0)..` (it replays what
  the wall clock has not made stale); and the lite-05 relay
  case #5000 left out: a live subscriber already reading group 1, then one
  from `Absolute(0)`, and a fresh group 0 reaches only the second. The closed
  #5085 branch has reusable pieces (older-wire codec mappings, the JS fix
  keeping groups below a floor buffered, mocked-time tests).
- Replace `requested_group` and `fetch_group` rather than keep both. Update
  every caller (relay, moq-archive, hls, the ladder) in the same PR,
  including the archive consumer's time resolution from its key listing if it
  has landed, plus the
  published surfaces built on them, per the cross-package table: moq-ffi
  (`fetch_group`, `requested_group`), moq-c (`poll_requested_group`), every
  binding wrapper and its `doc/lib` page, and `moq fetch` through
  `moq_relay::fetch_group` (added in the 2026-10-05 audit).
- Decided 2026-10-13 (#5400): the same reshape of `MoqSubscription` in moq-ffi
  (`rs/moq-ffi/src/consumer.rs`) adds a `MoqOrder` enum (`Ascending`,
  `Descending`) and `MoqSubscription.order`, defaulting to `Descending` and
  mapped to `track::Subscription::order`, beside the ranges. It follows the
  cross-package table like the rest: the `py/moq-rs`, `swift`, `kt`,
  `dart/moq` and `go/wrapper/moq` wrappers, `cpp/moq`, C (inherited if the
  [generated C bindings](/quest/m1/c/README.md) have landed, otherwise
  `rs/moq-c/src/api.rs`, from which cbindgen generates `moq.h`), and their
  `doc/lib` pages. `cpp/obs/src` and `doc/bin/obs.md` have no subscription
  surface today, so they change only if OBS starts building a
  `MoqSubscription`. Folded in here rather than a separate
  quest, so the bindings break once. Test that an ascending subscription
  receives a cached backlog oldest first in moq-ffi and each wrapper with tests.
- Decided in the 2026-10-10 audit: `max_delay` caps every range, including
  historical FETCH replacements, and publisher `max_age` always clamps it.
  A FETCH with no reader delay uses publisher max-age alone, not the live
  SUBSCRIBE zero-delay default. Preserve that distinction when translating
  FETCH into a range request. Sharing a range never relaxes a caller's
  budget or lets one expired caller cancel another still-valid reader.
  Reuse FETCH's optional per-reader budget and publisher clamp rather than
  restoring a historical exemption.
- Benchmark range count and span as separate axes (AGENTS.md fan-out rule).
