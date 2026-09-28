# [S] moqsink drops frames on recoverable importer errors

## Goal

A `moqsink` pad survives the importer errors moq-mux documents as
recoverable: it drops frames until the next keyframe instead of calling
`self.fail()` and losing the rendition for good. Two paths reach that
failure in `Media::write` (`rs/moq-gst/src/sink/pad.rs`) today:

- A new pad starts unlatched (`keyframe: false`), so a first buffer that is a
  delta frame fails it.
- A `TimestampRewind` with no signalled break (an rtspsrc jitterbuffer
  re-anchoring) has no guard at all.

## Plan

The header-only buffer after a break was fixed in
https://github.com/moq-dev/moq/pull/4356: the latch stays armed after a
successful decode. Reuse that latch here.

Decided: a video pad starts latched. On `TimestampRewind`, set the latch and
drop until a keyframe lands above the producer's live edge, losing up to one
GOP per re-anchor. Mapping a rewound timeline forward instead belongs to
[#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md).
The reporter offered the guard they run in production.

Regression tests beside `video_pause_drops_deltas_until_the_next_keyframe`:

- A new pad's first buffer is a delta: it drops.
- A rewind without a break: deltas drop until a keyframe above the edge.

## Closes

- [#4366](https://github.com/moq-dev/moq/issues/4366) - close this issue when the quest finishes
