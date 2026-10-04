# [M] A still screen share reaches late joiners

## Goal

A screen share whose content is not changing still shows its current picture
to a viewer or recorder that subscribes later, because the restarted encoder
has a frame to encode.

## Plan

`js/publish/src/fanout.ts` releases a frame with no readers and gives a new
reader only future frames. The video encoder (`js/publish/src/video/encoder.ts`)
rebuilds per demand, so after a demand gap it waits for the next frame, which a
still source never sends. `js/publish/src/video/capture.ts` keeps no latest
frame.

Decided (2026-10-04): `Video.Capture`'s fanout retains the latest frame and
hands a clone to each new subscriber, so a restarted encoder emits a fresh
keyframe at once. Most of the size is `VideoFrame` lifetime: the retained frame
is closed when replaced or when capture stops.

Tests: a fake source that emits one frame, then a subscriber that attaches
afterwards, produces a keyframe; no frame is leaked across replace and close.

## Closes

- [#4778](https://github.com/moq-dev/moq/issues/4778) - close this issue when the quest finishes
