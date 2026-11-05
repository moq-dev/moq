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
data. With a median queue build of about 4 minutes, one lane tops out near 13
merges an hour, and busy hours already run close to that.

Method: `sh/gh/queue-stats.py` (on this quest's branch) rebuilds occupancy from
the repository's `added_to_merge_queue`, `removed_from_merge_queue` and
`merged` issue events, removed and unmerged entries included. A never-empty
hour is a UTC clock hour the queue held an entry throughout; burst latency is
final enqueue to merge for PRs merged in those hours (nearest-rank p90). Builds
are the `Queue` jobs of `merge_group` runs that start in the window. Host load is the mean host-wide
CPU busy share that cargo's `--timings` sampled during each build's compile
phases (the runner's container shares the host's `/proc/stat`); builds that
compile nothing have none. The 2026-11-02 day reproduces the landing quest's
recorded 139 merges and 46.6/84.2 min all-merge latency. The earlier "p90
about 58 min, median 20.6" used an unrecorded definition and is not comparable.

Concurrency-1 baseline, 08:00Z to 08:00Z (midnight Pacific):

| Measure | 2026-11-02 | 2026-11-03 |
| --- | --- | --- |
| Never-empty clock hours | 9 | 7 |
| Merges per never-empty hour, median | 10.0 | 11.0 |
| Burst final enqueue to merge, median / p90 | 57.2 / 92.8 min | 49.7 / 76.0 min |
| All merges | 139 | 141 |
| Entries ahead at final enqueue, median / max | 9 / 31 | 9 / 19 |
| Queue builds (failed) | 156 (18) | 160 (18) |
| Failures per build | 11.5% | 11.2% |
| Build minutes, median / p90 | 4.3 / 9.5 | 4.0 / 8.8 |
| Host CPU busy in cargo phases, median / p90 | 65% / 85% (94 builds) | 51% / 75% (81 builds) |

Building two needs two runners. Only `nixos-moq-ci` carried the `moq-queue`
label, so at `maximumEntriesToBuild: 2` alone the second group's job would wait
for it and nothing would overlap. The host already runs at about half its CPU
during a single queue build, and building two means a failure in the first
entry rebuilds the second, so the trial reads failures, cancelled builds and
build minutes alongside the gain.

Decided 2026-11-05 (maintainer, /quest-unblock): the CI host's configuration
is a repository an agent may push to, and the host is reachable as
`ssh nixos`, so an agent applies the runner change itself.

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ copy the existing setup.
The second runner, `nixos-moq-ci2`, is a copy of `nixos-moq-ci`'s container
entry with its own home, work directory, registration token, UID range and
subnet. Like every runner it shares mbx's artifact store and the CI slice's
memory cap. It went live on 2026-11-05 at 08:40Z with the queue idle, and both
runners came back online. At concurrency 1 GitHub hands each Queue job to
whichever runner is idle, so the new runner warms its build directory before
the trial.

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ apply only when the merge
queue is idle: no entries and no `merge_group` build running. This covers both
the host switch and the ruleset trial.

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ the agent sets
`maximumEntriesToBuild` to 2 in ruleset 2420853 for one weekday, 08:00Z to
08:00Z, once the new runner has warmed its build directory, and reverts it to 1
when the window ends. A window that cannot start within the agent's run is
recorded under Waiting with its start, end and revert, and the ruleset stays
at 1.

Rerun after the trial window, with the baseline days for comparison:

```sh
sh/gh/queue-stats.py .tmp/queue-cache \
  2026-11-02T08:00:00Z 2026-11-03T08:00:00Z \
  2026-11-03T08:00:00Z 2026-11-04T08:00:00Z \
  <trial start> <trial end>
```

Comparable means a weekday with about as many never-empty hours and entries
ahead as the baseline days; a quieter one is extended by another day. Its
`overlapping build pairs` must be nonzero, or the setting never took effect.
cargo-timings artifacts expire after 14 days, so measure host load before
then. If 2 wins, `queue.yml`'s comments that a group is one PR at concurrency
1 change with it.

## Waiting

- moq-dev/moq: run the trial in
  [ruleset 2420853](https://github.com/moq-dev/moq/rules/2420853) from
  2026-11-06 08:00Z to 2026-11-07 08:00Z. At the start, check that
  `nixos-moq-ci2` has run Queue jobs (the `runner_name` of recent
  `merge_group` runs' jobs), wait until the queue has no entries and no
  `merge_group` build running, then set the `merge_queue` rule's
  `max_entries_to_build` to 2. At 2026-11-07 08:00Z set it back to 1, rerun
  the measurement above and keep the winner. Check: `gh api
  repos/moq-dev/moq/rulesets/2420853 --jq '.rules[] | select(.type ==
  "merge_queue") | .parameters.max_entries_to_build'` prints 2 inside the
  window and 1 outside it. If the start slips past the morning, move the whole
  window to the next weekday; never leave 2 without its revert time recorded
  here.
- Maintainer: keep `sh/gh/queue-stats.py` on `main`, or delete it once the
  comparison is recorded. Recommendation: delete it. Its only planned consumer
  is this trial's rerun from this branch, and a permanent tool would also need
  a test.
