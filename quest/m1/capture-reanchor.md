# [XS] Native capture re-anchors above its last timestamp

## Goal

A device clock that repeats or restarts never rewinds native video capture,
even while the pump drains a backlog faster than real time. `FrameChannel::push_native`
in `rs/moq-video/src/capture/channel.rs` re-anchors such a frame to its
arrival time ([#4125](https://github.com/moq-dev/moq/pull/4125)). During a
fast drain the previous frame's mapped timestamp can sit ahead of arrival, so
the re-anchor lands below it: source 0 and 40 ms arriving 1 ms apart publish
near 0 and 40 ms, and an immediate reset to zero publishes near 2 ms. Past a
closed group that is `TimestampRewind`, which stops capture.

## Plan

Remember the last mapped timestamp and floor the re-anchor strictly above it,
including the fallback when the checked arithmetic fails. The mapping keeps
advancing with the device clock from there.

Unit regression in `channel.rs`'s mapping tests: two frames drained 1 ms apart
with a 40 ms source step, then a source reset, maps above the second frame.
