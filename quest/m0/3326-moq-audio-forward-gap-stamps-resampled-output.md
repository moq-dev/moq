# [S] moq-audio: a forward timestamp gap stamps resampled output correctly

## Goal

Resampled audio that spans a forward gap in the source timeline is stamped
where its first sample actually belongs. Today a 10 ms packet at `t = 0`
followed by a packet at `t = 1s` produces a chunk stamped near `990 ms` whose
first samples come from `t = 0`, placing that audio almost a second late and,
since `Frame::activity` is picked by the frame's stamp, labelling active audio
as withheld.

## Plan

`decode::Consumer::starts_at` derives the chunk's start by subtracting the
samples the resampler still holds from the new packet's timestamp, which
assumes the held samples immediately precede it. A forward gap breaks that
and does not bump the container discontinuity counter, so nothing resets the
state. Only reachable when `decode::Config::sample_rate` differs from the
codec rate, since there is no resampler otherwise.

Track the source timestamp of the samples fed to the resampler alongside the
samples, so `starts_at` reads the held samples' real origin instead of
deriving it. A gap larger than the resampler's hold then produces two
correctly stamped chunks with a hole between them, which is the shape the
`dev` gap quest for the decode path (#2981) defines for gaps.

Tests: the 10 ms then 1 s case stamps the first chunk at `0`; contiguous
packets are unchanged; the activity label follows the corrected stamp.

## Closes

- [#3326](https://github.com/moq-dev/moq/issues/3326) - close this issue when the quest finishes
