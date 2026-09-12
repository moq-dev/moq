# [S] play: a tune-in burst must not stall behind the video queue

## Goal

`moq play --delay` above roughly a second reaches the live edge as quickly as
the default does. Today the picture can settle a further delay behind live and
stay there for the session.

`play_video` in `rs/moq-cli/src/play/media.rs` folds each frame into the playout
clock as it is read, then parks on `drained` once the queue holds
`MAX_VIDEO_FRAMES` (30). The clock only moves toward live when a frame arrives
earlier than the anchor predicted, so a parked decoder cannot observe the rest
of a tune-in burst. With `--delay 2s` at 30 fps the queue is a second of media,
the window presents nothing for two seconds, and by the time it drains the
remaining burst frames are late under the old anchor, which `Pacer` deliberately
refuses to re-anchor on. The anchor stays pinned wherever the first frame landed.

## Plan

The frames are raw decoded surfaces, so a queue sized by the delay is not the
answer: 30 frames of 1080p NV12 is already ~90 MB.

Two shapes worth weighing, and the choice is A/V policy:

- Separate observing arrivals from queueing them. The clock only needs the
  timestamp, so the read loop could keep draining the decoder and fold each
  arrival while the queue is full, holding only the newest frames.
- Drop from the queue instead of parking. `play_video` deliberately does not
  drop the oldest today, because during a catch-up burst the front frames are
  still ahead of the clock. A frame already past due under a moved anchor is a
  different case, and dropping those is what the window would do anyway.

Either way the regression test is a video-only burst larger than the queue with
a delay wider than the queue holds, asserting the clock ends up at the live edge
rather than a delay behind it.

## Related

- [Playout clock](https://github.com/moq-dev/moq/pull/3528) - added the clock this bounds
