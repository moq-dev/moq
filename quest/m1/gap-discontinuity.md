# [M] A gap in the group sequence is a discontinuity unless the boundary proves continuity

## Goal

A consumer resets codec state and its timeline whenever the group sequence it
delivers has a hole it cannot prove harmless, whatever made it: the publisher
skipped a sequence to declare a break, the relay or the subscriber's budget
shed a group, or a group was lost. A hole is harmless only when the previous
group's end meets the next group's first timestamp within the rounding
tolerance the js consumer already uses (`CONTIGUITY_TOLERANCE`), which keeps the legal
non-sequential numbering (DTS-derived ids, the HLS importer's packed epoch
bits) playing through. No signal that has to arrive carries the reset,
because moq-lite is lossy and any marker group can be shed before the
container layer sees it. An empty group stops meaning anything.

## Plan

Today `drafts/draft-lcurley-moq-hang.md` says an empty group declares a
discontinuity between codec epochs, and both container consumers key on it:
`rs/moq-mux/src/container/consumer.rs` waits for a zero-frame group's FIN in
`GroupBuffer::poll_empty` and bumps the counter in `mark_discontinuities`
(on the latency-skip path too); `js/hang/src/container/consumer.ts` does the
same through `Group.empty` and `#markDiscontinuity`. The one producer is
`rs/moq-mux/src/container/producer.rs` `Producer::discontinuity()`, which
appends and immediately finishes an empty group; its callers are the
h264, h265, and opus importers, `rs/moq-video`'s idle capture path
(`capture_stopped`), `rs/moq-audio`'s deferred `pending_discontinuity`, and
moq-boy. js/publish never emits one. On dev, `TrackState::is_stale` in
`rs/moq-net/src/model/track.rs` sheds a finished empty group under a zero
budget, since its reach is its successor's start, so a live-edge subscriber
never sees the marker and the reset is lost end to end (#3291).

- Draft: replace the empty-group sentence with the gap rule. A publisher
  declares a discontinuity by skipping at least one group sequence. A consumer
  MUST reset codec state, reapplying startup delay and pre-skip, before
  decoding the first group after a gap in the sequence it delivers, declared
  or not, unless the previous group's end meets the next group's first
  timestamp within a rounding tolerance of about a microsecond, since a
  1024-sample AAC frame has no integer microsecond duration and independently
  rounded stamps differ by one. Video knows its end from the
  [duration marker](/quest/m2/duration-marker.md); audio from its
  codec-defined frame durations. An empty group is permitted and carries no
  meaning. Group sequences stay free to be non-sequential.
  A contiguous boundary is what every ordinary group boundary already is:
  the keyframe that opens the next group resets prediction state and carries
  its parameter sets, and a codec config change rides the catalog, so an
  encoder replacement on a continuous clock needs nothing more. A publisher
  that wants the consumer to re-apply startup delay presents a hole.
- Consumers, both languages: `discontinuity()` bumps when the delivered
  sequence advances by more than one and the boundary is not contiguous
  (`ptsContiguous` in the js consumer is the shape, now exact rather than
  upper-bound), and the empty-group state machine
  (`poll_empty`, `Group.empty`, the FIN wait) goes. The max-age skip path
  already resets through the same counter. `rs/moq-audio`'s undeclared-hole
  handling from #3386 (`gap()`) becomes the declared path as well.
- Cost to check: a video decoder reset on every shed group. Measure a
  WebCodecs reset plus configure, and a VideoToolbox or openh264 reset, on the
  keyframe that follows a skip. If it is material, make the container-level
  reset a timeline reset and let the decoder decide whether the keyframe needs
  a flush.
- Gap gate: the mux consumer cannot tell a skipped sequence from a late group
  (#3258, `max_ts - next_start >= max_age`), so a declared skip resumes only
  when the gate trips or the timestamp jump already exceeds the budget. Decide
  by measurement whether `Producer::discontinuity()` keeps writing an empty
  group purely as a walk-now hint, harmless when shed, or stops; record the
  resume latency either way.
- Producers: `Producer::discontinuity()` skips a sequence, and every caller
  above follows; the importers re-estimate as today.
- Tests: the transport-level regression for #3291 (a zero-budget subscriber,
  an idle publisher, a resume: the reset happens); the mux tests
  `empty_group_declares_a_discontinuity`,
  `latency_skip_preserves_empty_group_discontinuity`, and
  `empty_group_advances`, with their js equivalents, rewritten for the gap
  rule; a shed group resets; an empty group does nothing; a non-sequential id
  jump with a contiguous boundary (`consumer.nonsequential.test.ts`) does not
  reset.

Branch from dev, where `is_stale` and the current consumers live.

## Required

- [Duration marker](/quest/m2/duration-marker.md) - a video group has to know its own end before a boundary can prove continuity

## Closes

- [#3291](https://github.com/moq-dev/moq/issues/3291) - close this issue when the quest finishes

## Related

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - requires this: a declared discontinuity is what a forward-only timeline continues from
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - the watch decoder reset that fires on the counter
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - the retention half of timestamp-only expiry
