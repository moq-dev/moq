# [M] js/json, the JSON stream producer, and moqsink stop matching on errors

## Goal

Three more places decide behavior by matching an error variant (AGENTS.md:
"Never match on errors"); each callee reports the case as a typed outcome
instead:

- `rs/moq-json/src/stream/producer.rs` `Inner::append`, and its JS twin
  `js/json/src/stream/producer.ts`: an oversized record
  (`FrameTooLarge`) keeps the log, any other encode failure aborts it. Give
  the stream `Encoder::encode` a refusal outcome, as the snapshot `Encoder`'s
  `Update::TooLarge` does.
- `js/json/src/snapshot/encoder.ts`: `update` and `snapshot` still throw
  `FrameTooLarge` for a value past the group budget. Mirror the Rust
  `snapshot::Update` outcome (`Unchanged`, `Frame`, `TooLarge`) and update
  `js/json`'s snapshot producer, which turns `TooLarge` back into the error.
- `rs/moq-gst/src/sink/pad.rs` `Media::write`: a `MissingKeyframe` while
  waiting for one drops the frame, and a `TimestampRewind` cuts the group.
  The codec importer reports both as outcomes; `container::Producer::rewind`
  already answers the rewind without writing.

## Plan

Found while doing [moq-dev/moq#9041](https://github.com/moq-dev/moq/pull/9041),
which fixed the five sites the 2026-11-17 pre-cut audit named. Neither breaks
today; each turns a future error change into a silent behavior change. Check
each with a test that drives the case end to end.

Public API: the stream `Encoder::encode` return type in Rust and JS, and the JS
snapshot `Encoder`'s `update` and `snapshot`. Wire: none.
