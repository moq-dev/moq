# [M] A TS export that learns its rate mid-stream keeps its audio on time

## Goal

`moq export ts` of a source whose multiplex rate the import records only after the export's
first output (a meter slower than the export's rate wait, or a rate that changes) never fails
with "MPEG-TS output missed a decode deadline" for its audio, and drops no frame an export
given the rate from the start would keep, at any `--delay`.

## Plan

[moq-dev/moq#9636](https://github.com/moq-dev/moq/pull/9636) sends audio ahead in a padded
multiplex as far as its decoder buffer fills at the multiplex rate (`audio_reach` in
`rs/moq-mux/src/container/ts/export.rs`), within half the delay. A rate learned mid-stream grows
that send-ahead only by the jitter buffer's spare (`jitter::Engine::spare_seen`), less a slot.
[moq-dev/moq#9839](https://github.com/moq-dev/moq/pull/9839) lets it grow freely until the clock
is anchored and uses the spare since the anchor in the first second. That fixed the long-delay
case (`ffmpeg_ac3.ts` at 2 s; the `ffmpeg_*.ts` sweep at 0.5 s, 2 s, and 3 s).

What remains is short delays. Below FFmpeg's ~186 ms PES packing, the source already makes frames
miss their deadlines, so the spare is negative and the send-ahead cannot grow. The export then
fails fatally at the recorded rate, while one given that rate from the start only drops the late
frames. Measured with `paced_export` and `Rate::Measured`:

- 100 ms: `aac`, `ac3`, and `mp2` fail with "missed a decode deadline … sent as far ahead as its
  decoder buffer holds; raise the multiplex rate". `Rate::Given` drops 31, 38, and 49 frames.
- 300 ms: `mp2` fails (spare −60 ms); `Rate::Given` drops 8.
- 300 ms: `eac3` drops one frame near the fixture's end. The spare over the last second (76 ms)
  overestimated what a later frame had, so the growth it allowed made that frame late. This
  predates #9839.

Lifting the cap entirely trades the fatal errors for drops at moderate delays (eac3 at 500 ms and
1 s), so it is not an option.

## Waiting

- Maintainer: When a rate is learned mid-stream at a delay too short for the source (frames already miss their deadlines, so the send-ahead cannot grow), what should the export do?
  - Fail loud naming the delay (Recommended) - refuse with an error that the delay is shorter than the source sends ahead, instead of the misleading "raise the multiplex rate"; at these delays an export given the rate drops a third of the audio anyway
  - Grow anyway once frames already miss - grow to what the rate needs when the spare is negative, matching the given-rate export's drops, at the cost of a rule that flips at zero spare
  - Pad above the recorded rate - raise the padded rate to what the capped send-ahead needs, keeping every frame the source delivers on time but departing from the recorded CBR, often several times over
