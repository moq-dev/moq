# [M] The shared clock follows a lasting path-delay step

## Goal

After a lasting increase in path delay, audio and video settle on the shared
clock at the playout delay again, in the browser and natively, instead of
audio resting behind a clock whose reference never moves later. The `step`
rows of the audio-quality harness show a steady-state `av_lag_ms` p50 within
a frame of zero.

## Plan

Decided 2026-11-11 (maintainer, /quest-merge pick of
[moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)): keep.

The stretch model deferred this so it would be decided with numbers. At
#8507's head the live `step` rows rested 35.2 and 41.8 ms behind in the
browser (AAC, Opus) and 5.7 and 17.3 ms natively, p95 37.5 to 65.1 ms, with
native AAC at 29.3 underruns a minute (614 under
[moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361)).

Recommendations:

- Let the reference move later only on sustained evidence, such as the
  minimum offset over a window well past the target, and treat the move like
  a target rise: audio glides to it by stretching, video holds a frame.
- Keep one rule in `js/watch` `Sync` and `moq_mux::sync`, with a shared test
  vector, as the stretch model's decisions corpus does.
- Do not add latency to reach zero underruns on the step.

## Required

- [Calibrate](/quest/m1/audio-jitter-target/calibrate.md) - the step rows' ceilings to measure against
