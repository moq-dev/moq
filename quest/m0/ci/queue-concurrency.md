# [S] The merge queue builds two entries at once if it measurably helps

## Goal

A measured trial decides whether moq's merge queue builds two entries at once
(`maximumEntriesToBuild: 2`) or stays at one. Done when the PR records both
settings' burst p90 enqueue-to-merge latency and median merges per
never-empty hour over comparable windows, and the maintainer keeps the winner
in the ruleset.

## Plan

Planned 2026-11-04 with the maintainer. The landing quest
([last version](https://github.com/moq-dev/moq/blob/17a81d6c64/quest/m0/ci/land.md))
ran the queue at build concurrency 1 and said to try 2 only with measured
data. Its performance goal closed on that data (decided 2026-11-04 ✅): over
2026-11-03 06:47Z to 2026-11-04 06:47Z, the eight hours the queue was never
empty merged a median of 11.0 PRs against the queue baseline's 6.0.

Why try 2: with a median queue build of 4.4 min, one lane tops out near 13
merges an hour, and busy hours already run close to that. Bursts of 22-26
entries on 2026-11-03 pushed p90 enqueue-to-merge to about 58 min (median
20.6 min), and waiting behind other entries was most of it.

Method: occupancy comes from the PRs' `AddedToMergeQueue`,
`RemovedFromMergeQueue` and `Merged` timeline events (reconstruct removed and
unmerged entries too). Compare at least one busy day at each setting, and
count queue failures per build, since building two means a failure in the
first entry rebuilds the second. The ruleset change and revert are the
maintainer's actions (ruleset 2420853); the quest names the setting and
reports the numbers. The host runner must handle two builds at once without
the shared mbx cache or `quest gate` admission erasing the gain, so record
host load during the trial.

The trial started early (decided 2026-11-05, maintainer, /quest-plan): the
second queue runner `nixos-moq-ci2` landed in
[kixelated/nixos#24](https://github.com/kixelated/nixos/pull/24), and ruleset
2420853 went to `max_entries_to_build: 2` at 2026-11-05T15:49:30Z. A scheduled
task sets it back to 1 at 2026-11-07T08:00Z. Measure the setting-2 window
against a comparable setting-1 window before it, such as
2026-11-03 06:47Z to 2026-11-04 06:47Z, and check the ruleset reads 1 again
before recording the result.
