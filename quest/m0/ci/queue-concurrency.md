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
are the `Queue` jobs of `merge_group` runs. Host load is the mean host-wide
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

Building two needs two runners. Only `nixos-moq-ci` carries the `moq-queue`
label, so at `maximumEntriesToBuild: 2` alone the second group's job waits for
it and nothing overlaps. A second runner in the same container would split one
CPU and memory budget between both builds; a second container gets its own
budget and shares mbx's action cache the way the host's runners already do.
The host already runs at about half its CPU during a single queue build, and
building two means a failure in the first entry rebuilds the second, so the
trial reads failures, cancelled builds and build minutes alongside the gain.

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

- Maintainer: register a second runner labeled `moq-queue` on the CI host, in
  its own container (recommended) or the existing one. The host configuration
  is private and deploys with a host rebuild, so an agent can draft it but not
  apply it. Check: `gh api repos/moq-dev/moq/actions/runners` lists two online
  `moq-queue` runners.
- Maintainer: set `maximumEntriesToBuild` to 2 in
  [ruleset 2420853](https://github.com/moq-dev/moq/rules/2420853) for one
  weekday from 08:00Z to 08:00Z once the second runner has warmed its build
  directory, such as 2026-11-05 08:00Z to 2026-11-06 08:00Z, then rerun the
  measurement above and keep the winner. Ruleset changes are the
  maintainer's. Check: `gh api repos/moq-dev/moq/rulesets/2420853 --jq
  '.rules[] | select(.type == "merge_queue") | .parameters.max_entries_to_build'`
  prints 2.
