# [M] The Firefox player passes its lifecycle every Nightly

## Goal

Nightly's `Media (firefox player)` (`just test media --media-engines firefox` on the
`ubuntu-24.04-arm` runner) passes every lifecycle step on every run, each failure fixed at its
cause, never by a looser bound, a retry, or a longer wait.

## Plan

Found by the Nightly triage of 2026-11-27. The job failed 6 of the 15 Nightly runs from 11-23 to
11-27, at a different step almost every time:

- [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (11-23): `live incumbent
  fallback plays`, `rms 0` with `audioStalled: false` within the 1039 ms bound.
- [45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582) (11-24): `lifecycle
  player returns`, a frame held 1100 ms behind the fixture against 1500 ms.
- [45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463) (11-26): `audio tone:
  after resume`, 65 of 75 samples.
- [45910365489](https://github.com/moq-dev/moq/actions/runs/45910365489) (`348692afc4`): `audio
  tone: after reattach`, 61 of 69 samples.
- [45989796092](https://github.com/moq-dev/moq/actions/runs/45989796092) (`1322b2f681`):
  `rejoin reaches live`, a frame held 1533 ms behind against 1500 ms, `audio running and
  buffering with nothing anchored`, last sample 302 ms old.

[moq-dev/moq#9620](https://github.com/moq-dev/moq/pull/9620) deleted the rejoin and reattach
child after 12 of 12 focused ARM runs passed. Nothing in the player changed after that
measurement, so focused runs miss what the full job hits. The 11-26 triage dropped the
`after resume` tone as a one-night failure; it has recurred since at other steps.

1. Reproduce on the ARM runner class with the whole `media` lane, not a focused step,
   dispatching only that job (never the full Nightly), until it fails at least twice. Keep the
   pages' clocks, the relay log, and the player's buffer state at each failing step.
2. Find whether the steps share a cause. Most failures read as audio that is running but
   unanchored, or short of tone after a resume: either the player's audio re-anchor after a
   lifecycle change, or a starved host. The harness's long-animation-frame count reads 0 on
   Firefox, which has no such API, so it rules out neither: tell them apart by lining the
   player's sample gaps up with the host trace the harness prints (`busiest host processes`).
3. Fix each cause with a failing test first, in `js/watch` where the player is at fault, on a
   paused clock.

The replacement case's history lives in [interop green](/quest/m0/interop-green/README.md),
including the lite-06 cold join's replay of about 2 s of retained audio, which waits for lite-07
(decided 2026-11-26). Rejected: a looser bound for lite-06.

Public API: none. Wire: none.
