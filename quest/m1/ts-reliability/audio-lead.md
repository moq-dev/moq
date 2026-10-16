# [S] A trimmed video start keeps audio behind the keyframe

## Goal

`moq export ts` begins on a video keyframe even when the video's start group
is dropped at acquisition and its next keyframe has not arrived yet. No audio
or data frame goes out ahead of the first video frame.

## Plan

Found reading [#5713](https://github.com/moq-dev/moq/pull/5713), not
reproduced. Until video goes out, `Export::leads_video`
(`rs/moq-mux/src/container/ts/export.rs`) drops non-video frames that present
before the earliest queued video frame. With no video frame queued it falls
back to `video_start`. When the acquisition drops the video's start group as
stale, the video queue is empty until the next keyframe arrives. Audio after
`video_start` then goes out first, by up to the remaining GOP less the delay.
The upstream-cut path that #5565 hit is not affected, because the next
group's keyframe arrives with the gap.

Recommended fix, settled in #5713: while no video frame has gone out and none
is queued, drop non-video frames as trimmed. Land it only with a regression
that fails without it. Three paused-clock attempts on the `Interleave` rig did
not drop the start group, because a frame's `arrived` is when its source was
last found empty. A fixture likely needs the tables to wait on something
other than the video (a late catalog entry), so the held keyframe is read
well before the rest of its group.
