# [S] A TS export that learns its rate mid-stream keeps its audio on time

## Goal

`moq export ts` of a source whose multiplex rate the import records only after the export's
first output drops no frame that an export given the rate from the start would keep, at any
`--delay` the source's audio fits in.

## Plan

[moq-dev/moq#9636](https://github.com/moq-dev/moq/pull/9636) sends audio ahead in a padded
multiplex as far as its decoder buffer fills at the multiplex rate (`audio_reach` in
`rs/moq-mux/src/container/ts/export.rs`), within half the delay. A rate learned mid-stream grows
that send-ahead only by the jitter buffer's spare (`jitter::Engine::spare_seen`), less a slot.
[moq-dev/moq#9839](https://github.com/moq-dev/moq/pull/9839) made two changes:
- The send-ahead grows freely until the clock is anchored, then by the spare seen since the anchor.
- At a delay shorter than the source sends its audio ahead, a rate learned mid-stream is refused
  with an error naming the delay (maintainer decision, 2026-11-28).

What remains: `ffmpeg_eac3.ts` with `Rate::Measured` at a 300 ms delay drops one frame (decode
4088 ms) that `Rate::Given` keeps. The rate arrives about 1.8 s in, and the spare then (76 ms)
allows 51 ms of growth. The dropped frame shares an export PES with an earlier frame, so it takes
that frame's key (`Track::release_key`) and has less spare than any frame in the second before.
The likely cause is that FFmpeg's 186 ms PES cadence beats against the export's ~160 ms PES span
over more than the spare's one-second window. Taking the least spare over two seconds did not help,
since only one second had passed when the rate arrived. Check that hypothesis first, for example by
bounding growth by the spare less the PES span that `Track::pack_start` gives at the delay.
Reproduce it with a `paced_export` sweep over the `ffmpeg_*.ts` fixtures.

This quest does not gate 0.22.0. It stays in m1 only while #9839 holds its claim; once that claim
ends, move it to m3 with `quest mv`.
