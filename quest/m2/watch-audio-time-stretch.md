# [M] js/watch: the audio ring converges by time-stretching instead of skipping or going silent

## Goal

When the playout target moves or the ring drifts from it, audio catches up
or holds back by playing slightly faster or slower, the way NetEq's
accelerate and preemptive expand do, rather than by discarding buffered
audio or rendering silence. Convergence is inaudible at ordinary drift and
burst sizes.

Boundaries: no packet loss concealment; an underrun still renders a ramped
gap. The target estimator and the ring's slack and re-stall are the
[audio jitter target](/quest/m1/audio-jitter-target/README.md) line (#3517
was closed in favor of it); the clock the stretch converges toward is
[A/V clock](/quest/m1/av-clock.md).

## Plan

- Implement WSOLA-style stretch and compress in `render-worklet.ts` on the
  PCM the ring hands out, bounded to a few percent per quantum, driven by the
  distance between buffered audio and the target. Skip-ahead remains only
  for a distance larger than the stretch can close within a bound.
- Both rings expose the distance the same way, so the worklet code is shared
  between the isolated and the postMessage paths.
- Verification: replay the recorded arrival traces
  (`test/audio-quality/traces/`, through `js/watch/src/audio/replay.ts`)
  and assert zero skips and zero underruns after convergence, plus a
  listening check that a 2 ms/s drift is inaudible.

## Required

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the recorded traces this replays, and the JS target it converges toward
- [A/V clock](/quest/m1/av-clock.md) - the clock the stretch converges toward
