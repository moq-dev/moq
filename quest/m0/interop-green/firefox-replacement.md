# [M] Firefox plays a live replacement and its fallback within the bound

## Goal

`Media (firefox player)` in the scheduled Nightly passes the `replacement`
case on every wire: the viewer shows the new run's picture and tone within
1039 ms of its announcement while the incumbent stays open, and the still-live
older run's within 1039 ms of the newest withdrawing.

## Plan

The case failed on Firefox every night from 2026-11-19 to 2026-11-23. Two
causes stacked:

- Starvation, fixed in [moq-dev/moq#9494](https://github.com/moq-dev/moq/pull/9494):
  the 4-vCPU `ubuntu-24.04-arm` runner, decoding in software, ran five media
  pages through the section, and the lifecycle player underflowed with the
  replacement viewer every night. The lifecycle player now detaches and the
  lifecycle fixture stops encoding video for the section. Measured on that
  runner class with focused runs
  (`just test media --media-engines firefox --media-cases replacement`): 4 of 20
  passed without it, 29 of 30 with it.
- What remains: the `moq-lite-06` fallback, at 719 to 1028 ms on ARM against
  the 1039 ms bound (lite-07's 314 to 438 ms, moq-transport's 214 to 319 ms;
  [run 45309593159](https://github.com/moq-dev/moq/actions/runs/45309593159),
  20 of 20 passed). The relay's trace shows the viewer's audio join as
  `Live { min: None, max: Some(2.13s) }`: a lite-06 SUBSCRIBE states no lag,
  and the player's Max Age reaches 2 s past its delay so its jitter estimator
  sees every arrival. The incumbent's audio (`onDemand: "retain"`) then
  encodes those 2.13 s of retained PCM in time order before its first live
  frame, about 300 ms of Opus on ARM, and the viewer skips that history as it
  trails the clock (`skipping missing groups: track=audio`).
  [moq-dev/moq#9511](https://github.com/moq-dev/moq/pull/9511) measured that
  replaying nothing for such a join brings the fallback to 217 to 323 ms, but
  the maintainer decided (2026-11-23) that a join with no lag limit replays the
  retained audio within its Max Age, as `main` already did.

Next: a maintainer decision on how to remove the replay's cost under that rule
(see #9511's `## Waiting`). Loosening the 1039 ms bound or retrying is not a
fix. A temporary `pull_request` workflow running the focused case five times per
`ubuntu-24.04-arm` job, as #9494's and #9511's did, measures it.

Public API: none expected. Wire: none expected.
