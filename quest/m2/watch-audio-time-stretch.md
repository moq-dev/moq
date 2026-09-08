# [M] js/watch: the audio ring converges by time-stretching instead of skipping or going silent

## Goal

When the playout target moves or the ring drifts from it, audio catches up
or holds back by playing slightly faster or slower, the way NetEq's
accelerate and preemptive expand do, rather than by discarding buffered
audio or rendering silence. Convergence is inaudible at ordinary drift and
burst sizes.

Boundaries: no packet loss concealment; an underrun still renders a ramped
gap. The target estimator and the ring's slack and re-stall are #3517 on
dev; the clock the stretch converges toward is
[Plan: A/V clock](/quest/m1/plan-av-clock.md).

## Plan

Branch from dev.

- Implement WSOLA-style stretch and compress in `render-worklet.ts` on the
  PCM the ring hands out, bounded to a few percent per quantum, driven by the
  distance between buffered audio and the target. Skip-ahead remains only
  for a distance larger than the stretch can close within a bound.
- Both rings expose the distance the same way, so the worklet code is shared
  between the isolated and the postMessage paths.
- Verification: replay the recorded arrival traces from the auto-latency
  quest and assert zero skips and zero underruns after convergence, plus a
  listening check that a 2 ms/s drift is inaudible.

## Required

- [Plan: A/V clock](/quest/m1/plan-av-clock.md) - stretching against a free-running ring only moves the drift

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - the recorded traces this replays
