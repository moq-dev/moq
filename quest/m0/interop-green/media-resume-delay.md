# [L] A viewer's resume doesn't freeze playback on a late video burst

## Goal

The interop `media` step passes under CPU load: after a pause and resume, or a codec rename, audio and video play without a freeze inside the measurement window, in the scheduled Interop run and Nightly's `Media (firefox player)` job. Both have failed every day since 2026-11-12.

## Plan

Both fail `FAIL audio tone: after resume: N/81 samples carried the fixture tone` in "media output + lifecycle". The `remote error: 1`/`54`, `stream aborted` and `fetch error … not found: group 10` lines are the expected cancels of pause and rename, also present in passing runs.

The first bad commit was [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507) (`4262a0691e`; after resume, audio caught up by playing up to 1.6x). That looks fixed by [moq-dev/moq#8675](https://github.com/moq-dev/moq/pull/8675): the pause case alone under load now passes 4/4.

What remains on `main` fails about 25% under load: after resume, the first frames of a new video group arrive together 400–900 ms late, the jitter estimator (`js/hang/src/container/jitter.ts`) counts that as path delay, and the auto delay jumps from 162 ms to 700–880 ms, so audio and video freeze about 0.5 s. The last good commit showed the same bursts, smaller (180–280 ms), so this may be old flakiness made visible.

Steps:

1. Find whether the burst is real (the Chromium publisher's keyframe encode stalling under load) or a re-subscription artifact the estimator should treat as join backlog. Its join rule excludes only frames produced before the first arrival. Log per-group arrival at the publisher, relay and player.
2. Decide what a large auto-delay rise does during playback: today it pads silence and freezes video for the whole step, while #8507 meant the ring to slow onto a rise. Coordinate with [audio playout](/quest/m2/audio-playout/README.md), which owns that rule.
3. Explain the 11-17 CI trace: audio reports timestamp 0 and is silent 0.6 s while video plays, then video freezes and jumps 600 ms. Why does `measurePlayback`'s "playback starts" wait pass while the ring has no audio yet?

Reproduce from `test/`: `just media --media-cases rename,pause` under `taskset -c 0-3` with two busy loops on the same cores, and `HARNESS_RUNS` set to a short path (pulse.py's Unix socket path is too long from a worktree's `.tmp`). Without load it passes almost every time (12/13).

Fix the root cause; no retries or looser thresholds.

Public API: none. Wire: none.
