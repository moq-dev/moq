# [M] A TS export that learns its rate mid-stream keeps its audio on time

## Goal

`moq export ts` of a source whose multiplex rate the import records only after the export's
first output (a meter slower than the export's rate wait, or a rate that changes) never fails
with "MPEG-TS output missed a decode deadline" for its audio and drops no frame, at any
`--delay`.

## Plan

[moq-dev/moq#9636](https://github.com/moq-dev/moq/pull/9636) sends audio ahead in a padded
multiplex as far as its decoder buffer fills at the multiplex rate (`audio_reach` in
`rs/moq-mux/src/container/ts/export.rs`), within half the delay, which carries every codec at the
recorded rate plus a packet a slot. A rate learned mid-stream grows that send-ahead only by the
jitter buffer's margin over the last second, and the tables carry it only once the frames keyed
for it are due (`Program::rate_from`).

What remains: while the margin is still unknown (the first second of output), the send-ahead does
not grow at all, so a slow multiplex misses. `an_ffmpeg_audio_source_exports_at_its_recorded_rate`
with `Rate::Measured` and a 2 s delay reproduces it on `ffmpeg_ac3.ts`. A frame's jitter key is
fixed when it arrives, so a send-ahead that grows has to come out of what the frames have to
spare or move the clock's anchor.

Decided 2026-11-26 (maintainer, /quest-run): let the send-ahead grow freely until the jitter
buffer releases its first frame, since its clock anchors on the latest track then, and check whether that anchor really stays
open until the first release.
