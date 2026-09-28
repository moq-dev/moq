# [S] moqsink drops frames on recoverable importer errors

## Goal

A `moqsink` pad survives the importer errors moq-mux documents as
recoverable: it drops frames until the next keyframe instead of calling
`self.fail()` and losing the rendition for good. Three paths reach that
failure in `Media::write` (`rs/moq-gst/src/sink/pad.rs`) today:

- After a pause or segment break, a header-only buffer decodes without
  emitting a frame, clears the `keyframe` latch, and the next delta hits
  `MissingKeyframe` outside the guarded arm
  ([#4239](https://github.com/moq-dev/moq/pull/4239)).
- A new pad starts unlatched (`keyframe: false`), so a first buffer that is a
  delta frame fails it.
- A `TimestampRewind` with no signalled break (an rtspsrc jitterbuffer
  re-anchoring) has no guard at all.

## Plan

Keep the latch set until the importer actually publishes a keyframe, not
until the first buffer parses. That needs `import::Track::decode` to say
whether it emitted a frame, or a comparable signal; prefer reusing what the
importer already knows over inferring it in the pad.

Decided: a video pad starts latched. On `TimestampRewind`, set the latch and
drop until a keyframe lands above the producer's live edge, losing up to one
GOP per re-anchor. Mapping a rewound timeline forward instead belongs to
[#3021](/quest/m1/3021-moq-gst-anchor-generated-media-timelines-to-wall-clock.md).
The reporter offered the guard they run in production.

Regression tests beside `video_pause_drops_deltas_until_the_next_keyframe`:

- A break, then a header-only buffer (SPS/PPS alone for H.264), then a delta,
  then a keyframe: the delta drops, the keyframe publishes, and the pad stays
  valid.
- A new pad's first buffer is a delta: it drops.
- A rewind without a break: deltas drop until a keyframe above the edge.

## Closes

- [#4366](https://github.com/moq-dev/moq/issues/4366) - close this issue when the quest finishes
