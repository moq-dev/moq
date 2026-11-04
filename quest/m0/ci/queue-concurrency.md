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
