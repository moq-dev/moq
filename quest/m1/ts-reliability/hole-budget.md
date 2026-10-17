# [S] Sequence holes get a time budget

## Goal

A container consumer waiting on a missing group sequence gives up on it once
a budget expires on max(wall clock, pts), then reports the gap and what
follows. A break marker behind a hole on a paused track is delivered without
waiting for media to resume or the track to finish. Rust
(`rs/moq-mux/src/container/consumer.rs`) and JS
(`js/hang/src/container/consumer.ts`) agree.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
review of [#5815](https://github.com/moq-dev/moq/pull/5815). The budget covers
every kind of hole, not only one before a data marker, and runs on
max(wall, pts) so it fires on a paused track.

Today a marker carries no timestamp, so a hole before it on an open, paused
track waits indefinitely. A buffered higher sequence alone proves nothing,
since a late group is not a lost one; the budget is what decides.

The two anchors, matching the timers the TS line already uses:

- PTS: the existing age measure, how far the newest content
  (`max_timestamp`) runs past where the next stamped group begins
  (`next_start`), in `rs/moq-mux/src/container/consumer.rs`, and its
  counterpart in `js/hang/src/container/consumer.ts`. A bare marker has no
  timestamp, so this side never fires behind one.
- Wall: the `moq_clock` instant the reader first found itself blocked on
  the hole, the way the TS export anchors a frame's arrival at the instant
  its source was last found empty (`Track::empty` in
  `rs/moq-mux/src/container/ts/export.rs`, judged by `jitter::Buffer` in
  `rs/moq-mux/src/jitter.rs`). A timer wakes the reader at the anchor plus
  the budget, with no new group needed.

The hole is lost once either side reaches the budget. Open for the
maintainer: the threshold. Recommendation: the subscription's max delay,
which already drives latency skips, so both sides share one number.

Test with mocked time in both languages: read group N, lose N+1, receive a
closed marker as N+2 on a paused track; after the budget the reader reports a
gap, then the end. It fails without the fix.
