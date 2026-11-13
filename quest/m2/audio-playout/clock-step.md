# [M] The shared clock follows a lasting path-delay step

## Goal

After a lasting increase in path delay, audio and video settle on the shared
clock at the playout delay again, in the browser and natively, instead of
playing against a reference that never moves later. The `step` rows of the
audio-quality harness rest within a frame of the clock after the step, with
no more underruns than before it.

## Plan

Decided 2026-11-11 (maintainer, /quest-merge pick of
[moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)): keep.
Re-scoped 2026-11-13 (maintainer, /quest-plan) to the
[audio playout](/quest/m2/audio-playout/README.md) rule, under which a move
of the clock is lag like any other.

The reference is the earliest lateness any track has shown, and arrivals only
move it earlier (`js/watch/src/sync.ts`, `rs/moq-mux/src/sync.rs`), while the
estimator measures each frame against the fastest of the last 2 s. After a
lasting step the estimate forgets the step within its window, but the clock
keeps the old reference, so every frame is late for it. On the 2026-11-09
rule audio rested behind: at #8507's head the `step` rows rested 35.2 and
41.8 ms behind in the browser (AAC, Opus) and 5.7 and 17.3 ms natively; a
simulated +200 ms step left native audio 150 to 160 ms behind. Under the
[lag rule](/quest/m2/audio-playout/lag-rule.md) audio follows the clock
instead, so the same stale reference shows as underruns.

Recommendations:

- Let the reference move later only on sustained evidence, such as the
  least lateness over a window well past the target staying past it. The
  move is lag like any other: audio slows down within the threshold or pads
  past it, and video holds a frame.
- Keep one rule in `js/watch` `Sync` and `moq_mux::sync`, with a shared test
  vector, as the estimator's corpus does.
- Do not add latency to reach zero underruns on the step.

## Required

- [Lag rule](/quest/m2/audio-playout/lag-rule.md) - the controller that treats a reference move as lag
