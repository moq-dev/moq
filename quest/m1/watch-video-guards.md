# [S] Watch video holds its picture and its order

## Goal

Two `js/watch/src/video/decoder.ts` defects stop reaching the viewer:

- Promoting a video track when none is active (after pause, scrolling back
  into view, or a hidden rendition returning) holds the last picture instead
  of painting black for a round trip plus a keyframe.
- An older finished group accepted behind the live group is never fed to the
  codec between live deltas.

## Plan

Decided:

- `#runActive` ignores the new track's initial `undefined` frame; clearing
  stays with `#clearCurrentFrame` and `close()`. The held timestamp stays
  with the held frame, so the outputs describe what is on screen.
- The guard lives in both video decode loops (legacy and CMAF): skip a frame
  whose group is older than the newest group already fed, and reset on a
  discontinuity. The consumer keeps serving older groups, since audio's
  timestamp-indexed ring needs them. Fix the consumer comment claiming video
  drops a late frame at render; that only holds after decode.

Tests: a promote from nothing keeps the previous frame and timestamp; an older
group arriving after a newer one never reaches the decoder.

## Closes

- [#4338](https://github.com/moq-dev/moq/issues/4338) - close this issue when the quest finishes
- [#4339](https://github.com/moq-dev/moq/issues/4339) - close this issue when the quest finishes

## Related

- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - another fix in the same decoder
- [Watch decoder recovery](/quest/m1/watch-decoder-recovery.md) - a bad feed today ends video for the subscription
