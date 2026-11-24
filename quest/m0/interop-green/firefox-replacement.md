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
- What remains: the `moq-lite-06` fallback. With the runner no longer starved,
  every other step takes 130 to 420 ms on ARM, but the lite-06 fallback takes
  716 to 1026 ms (x86: 308 ms), and its one failure of 30 ended with the
  incumbent's picture playing and its audio unanchored (`audioStalled: true`,
  `audioTimestamp: 0`) after two `skipping missing groups: track=audio` on the
  viewer. The lite-06 replacement, which also builds a fresh subscription,
  takes 157 to 284 ms, so the extra cost is the fallback's, not the wire's.
  The relay's default log shows no `err=old` abort in that window, unlike
  [reattach-old-groups](/quest/m0/interop-green/reattach-old-groups.md).

Next: capture a failing lite-06 fallback on the ARM runner with the relay at
`moq_net=trace` and the viewer's Playwright trace, to see which audio groups
the relay sent the lite-06 viewer and when, and why they were missing; then fix
the player or relay path that lets them go. Loosening the 1039 ms bound or
retrying is not a fix. A temporary `pull_request` workflow running the focused
case five times per `ubuntu-24.04-arm` job, as #9494's did, measures it.

Public API: none expected. Wire: none expected.
