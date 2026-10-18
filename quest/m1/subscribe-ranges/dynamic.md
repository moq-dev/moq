# [L] Dynamic range requests

## Goal

A publisher's `track::Dynamic` receives range requests for misses instead of
one request per group, and a track serves a subscription's ranges from one
cursor, in its order, without duplicating a group, naming every sequence in
range it won't deliver. A range end may be a `Time(pts)`, resolved by
whoever holds the track. `requested_group` and `fetch_group` are replaced, not
kept beside it.

## Plan

`track::Subscription` already carries aggregated `ranges`, but only as a wire
preference: locally a subscriber's ranges set where its reads start, and it
still reads every later group the aggregate brings in. Serving a range locally
means filtering a subscriber's reads to its own ranges, and a miss below the
cache becomes a range request on `Dynamic` rather than a group request.

- Add a `Time(pts)` anchor beside `Absolute` and `Relative` (see the
  [line](/quest/m1/subscribe-ranges/README.md)), carried through moq-ffi and
  every binding like the other anchors. A track resolves it from its cached
  groups' timestamps and passes an unresolved one to its `track::Dynamic`
  handler; an untimed track refuses it. Test a time start and end resolved
  against the cache, an unresolved one passed through to a handler, and an
  untimed track refusing one.
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
  [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) rather than adding a second one.
- Decided in the 2026-10-10 audit: `max_delay` caps every range, including
  historical FETCH replacements, and publisher `max_age` always clamps it.
  A FETCH with no reader delay uses publisher max-age alone, not the live
  SUBSCRIBE zero-delay default. Preserve that distinction when translating
  FETCH into a range request. Sharing a range never relaxes a caller's
  budget or lets one expired caller cancel another still-valid reader.
  Reuse FETCH's optional per-reader budget and publisher clamp rather than
  restoring a historical exemption.
- Move every relative-start resolution into the model, which counts back
  from produced groups only. Today two consumers re-resolve relative starts
  on their own. moq-c's `apply_raw_subscription` (`rs/moq-c/src/consume.rs`)
  counts back from `Ordered::latest()`, the shared group and datagram
  sequence, so a datagram past the newest group skews the start. It also
  re-resolves on every update, because the model doesn't. moq-mux's
  `container::Consumer::new` treats a relative start as an unfloored join
  and adopts the first arrival, which under newest-first delivery can skip
  the older groups the range asked for.
- Benchmark serving and filling over range count and span as separate axes,
  beside `track_subscription_ranges`, which covers aggregation.

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - publishers name every group they won't deliver
