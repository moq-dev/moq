# [M] The JS interop publisher's tone survives every draft

## Goal

Interop's `js ->` cells pass the audio check on every moq-transport draft
again, with the cause of the 2026-10-23 regression fixed at its source.

## Plan

The scheduled Interop run on 2026-10-23
([run 39958710886](https://github.com/moq-dev/moq/actions/runs/39958710886),
`--all --versions ietf`) failed every `js ->` cell on drafts 18 and 22, and
`js -> rust` and `js -> c` on draft 20, with "wrong tone: channel 0 carries
~440 Hz, not 440 Hz" at 0.10 to 0.77 of the energy at 440 Hz. `rust -> js`
also failed on drafts 20 and 22. The same job passed all of these on
2026-10-22, and drafts 14 and 16 pass on both days, so it is not a load
flake. It started after
[moq-dev/moq#6299](https://github.com/moq-dev/moq/pull/6299) merged (pause
and resume as a new subscriber), which is a guess from timing only.

Reproduce one cell locally, bisect between the two runs, and fix the cause.
Separate from [tone grace](/quest/m1/test-flakes-2/tone-grace.md), which
covers a false "wrong tone" under CPU starvation.

Decided 2026-10-23 (maintainer): its own quest, ranked above the load flakes.

Public API: depends on the cause. Wire: depends on the cause.
