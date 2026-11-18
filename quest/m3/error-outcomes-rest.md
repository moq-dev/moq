# [S] The JSON stream producer and moqsink abort on any error

## Goal

Two more call sites decide behavior by matching an error variant (AGENTS.md:
"Never match on errors"). Each stops matching and ends its output on any
error, as [moq-dev/moq#9041](https://github.com/moq-dev/moq/pull/9041) did for
moq-stats and the HLS import:

- `rs/moq-json/src/stream/producer.rs` `Inner::append`, and its JS twin
  `js/json/src/stream/producer.ts`: an oversized record (`FrameTooLarge`)
  is refused and keeps the log today, while any other encode failure aborts
  it. Abort the track on any encode failure, an oversized record included,
  and update both doc comments that promise the log survives one.
- `rs/moq-gst/src/sink/pad.rs` `Media::write`: a `MissingKeyframe` while
  waiting for one drops the frame, and a `TimestampRewind` cuts the group and
  carries on. Fail the pad on any decode error instead, so a rewinding source
  errors the pipeline rather than publishing around it.

## Plan

The maintainer settled the approach on #9041: abort on any error rather than
reporting typed outcomes, and leave any retry to another layer.

moqsink still has to skip the deltas GStreamer hands it before the first
keyframe, which is normal at pipeline start rather than an error. Recommended:
drop them by the buffer's `DELTA_UNIT` flag before decoding, so the importer
never sees them, rather than asking the importer and reading its error.

Check each with a test that drives the case end to end: an oversized stream
record ends the track in Rust and JS, and moqsink fails on a rewind.

Public API: none, but both stream producers change behavior: an oversized
record ends the log. Wire: none.
