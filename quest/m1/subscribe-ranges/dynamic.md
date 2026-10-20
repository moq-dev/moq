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
