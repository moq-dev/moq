# [S] An audio encoder flushes without ending its track

## Goal

An audio encoder's caller can flush the samples it holds into the track and
keep the track open, so the last partial frame of a clip is heard when the
source pauses rather than dropped.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`encode::Producer::finish` (`rs/moq-audio/src/encode/producer.rs`) drains and
then ends the track, and `discontinuity` drops buffered samples. The
non-consuming `Encoder::drain` is `pub(super)`
(`rs/moq-audio/src/encode/encoder.rs`). A studio media file's sound
([moq-dev/studio#44](https://github.com/moq-dev/studio/pull/44)) loses its last ~26 ms when a clip ends but the
source stays live. Related: [An audio track's final flush joins its open
group](/quest/m4/audio-flush-group.md).
