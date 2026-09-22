# [L] FlatBuffers stats flavor

## Goal

`moq_stats::Producer` serves `[<tier>/]{publisher,subscriber,sessions}.fb.z`
on request next to the JSON tracks, and `moq_stats::Consumer` and the
aggregate read it, all from a checked-in `.fbs` schema. A benchmark shows
`.fb.z` beating `.json.z` on bytes, CPU, and allocations, for both producer
and consumer. If it
does not clearly win, abandon the quest and report the numbers.

## Plan

- The schema in `rs/moq-stats` is the contract: one table per `Traffic` and
  `Presence`, keyed entries per frame, and room for the client-stats
  extension as an optional nested table. Fields are only ever appended.
  Generate with planus. Prefer generating in `build.rs` if planus supports
  it cleanly; otherwise check in the output with a just recipe and a CI diff
  check.
- Encode straight from the reused report of the
  [allocation-free tick](/quest/next/stats-binary/tick.md) into one planus
  builder that resets every frame. Each group is one DEFLATE window, as
  `.json.z` does today, with a full snapshot per frame and no deltas.
- `.fb.z` is a flavor suffix: it applies to every track name that takes
  `.json.z` (default and named tiers, sessions, and the per-broadcast tracks
  if [schema](/quest/next/qos/stats/schema.md) lands first) and nothing
  else. Extend `requested_track_shape` and the track-name helpers for it. Decide whether the helpers take a flavor enum instead of
  `compressed: bool`, and weigh that break while on dev.
- `Consumer` reads either flavor into the same frame types; decoding views
  the inflated buffer without copying strings until a caller keeps one.
- Benchmark `.json.z` against `.fb.z` over broadcasts x tiers: bytes after
  flate, encode and decode ns, and allocations on both sides. Put the numbers
  in the PR.
- Update the stats section of `doc/bin/relay/config.md` and the moq-stats
  crate docs (wire format) inline.

Public API impact: additive on moq-stats unless the helper signatures change
(dev). Wire impact: new on-demand tracks; the existing tracks are unchanged.

## Required

- [Allocation-free tick](/quest/next/stats-binary/tick.md) - the reused report the encoder reads from
