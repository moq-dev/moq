# [M] play: a tune-in burst must not stall behind the video queue

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

Chosen A/V policy: buffer encoded frames for the whole `max_age` window,
decode only a few ahead of presentation, and evict the oldest decoded frame
when that small queue fills. Video keeps observing the live edge without moving
the anchor while audio owns it. `moq_video::decode::Consumer` owns both the
container reader and the native decoder today, so split or compose them without
duplicating the subscription and codec rules. Bound the encoded buffer by media
age and account for its bytes; `--delay` allows 10s, which as raw 1080p frames
would be ~900 MB.

The regression lives on #3946's branch (`quest/main/play-tunein-backpressure`,
`play::media::tests`): 61 frames at 30fps, a 2s delay, and no window drain park
the decoder at frame 31, leaving the newest frame due 990ms late. Port it onto
the harness, and also cover video-only and speaker-owned anchors, delayed
drains, reordering, discontinuity, and the decoder's tail flush.

## Required

- [Play harness](/quest/m1/play-harness.md) - the regression test runs on it

## Related

- [Playout clock](https://github.com/moq-dev/moq/pull/3528) - added the clock this bounds
