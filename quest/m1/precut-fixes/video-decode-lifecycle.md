# [S] moq-video keeps a boundary through a failed drain and bounds its colour queue

## Goal

The video decoder and capture keep their lifecycles whole: a Gap or End is
delivered and leading-picture recovery is armed even when the drain before
it fails; the colour-switch queue stays bounded by the decoder's reorder
depth whatever a publisher's keyframes signal; and capture flushes its
encoder before dropping it, so the frames it holds are published.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; reproduce
first (inject a flush error through the `probe` backend).

- `rs/moq-video/src/decode/consumer.rs`: at a Gap or End, `self.drain().await?`
  returns before queueing `Read::Gap`/`Read::End` and setting `recover`, and
  the marker is already consumed. Keep the drained pictures ahead of their
  boundary: take the drain's result without `?`, queue whatever it drained,
  then the boundary, arm recovery, and only then return the drain's error.
  Read the end-of-track branch, which already survives a failed drain. From
  [moq-dev/moq#6475](https://github.com/moq-dev/moq/pull/6475).
- `rs/moq-video/src/decode/decoder.rs`: an entry pushes per keyframe whose SPS
  changes colour space and pops only when a delivered picture reaches it; a
  publisher alternating colour spaces with a dropped first picture grows it
  until the next Gap. Drop entries at or after a new keyframe's timestamp and
  cap at the reorder depth, as `arrivals` is. From
  [moq-dev/moq#5808](https://github.com/moq-dev/moq/pull/5808).
- `rs/moq-video/src/encode/producer.rs` `capture_loop`: `drop(encoder)` with
  no `flush()`, against the crate's own rule from #8372; Media Foundation and
  MediaCodec lose their last frames on a source end or resize. On `release`
  too.

Public API: none. Wire: none.
