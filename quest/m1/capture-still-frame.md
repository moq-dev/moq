# [M] A still screen share reaches late joiners

## Goal

A video source whose content is not changing still shows its current picture
to a viewer or recorder that subscribes later, published at the time it is
sent, so the encoder has a frame to encode and the jitter estimate is
untouched.

## Plan

`js/publish/src/fanout.ts` releases a frame with no readers and gives a new
reader only future frames. The video encoder (`js/publish/src/video/encoder.ts`)
rebuilds per demand, so after a demand gap it waits for the next frame, which a
still source never sends. `js/publish/src/video/capture.ts` keeps no latest
frame.

Decided (2026-10-04):

- `Video.Capture` keeps the latest frame from every video source and hands a
  copy to each new subscriber. Capture-specific, not a generic `Fanout`
  option, because audio must never replay. It costs one held buffer from the
  capture pool.
- The copy is re-stamped to now. A kept frame re-encoded 90 s later would
  otherwise publish 90 s late, and the jitter estimator keeps its maximum for
  the life of the stream, the same failure as DTX.
- The held frame is closed when replaced or when capture stops.

Tests: a fake source that emits one frame, then a subscriber that attaches
later, produces a keyframe stamped near the attach time; no frame is leaked
across replace and close.

## Closes

- [#4778](https://github.com/moq-dev/moq/issues/4778) - close this issue when the quest finishes
