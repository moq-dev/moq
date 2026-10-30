# [L] Gate each diff in under five minutes

## Goal

`just check` and `just test` each finish in under 5 minutes of wall time for
every diff-scoped run on the desktop with warm caches, counting time queued in
`quest gate` admission. A worker agent then runs its own gate inside its
5-minute prompt-cache window. Explicit full runs (`--all`, nightly, release)
are exempt; work moved out of the diff-scoped gate goes to the merge queue or
nightly with its coverage kept.

## Plan

Decided 2026-10-29 (user): sub-agents run with a 5-minute prompt cache and the
coordinating session with a 1-hour one
([kixelated/quest#276](https://github.com/kixelated/quest/pull/276)). A worker
that waits longer than 5 minutes on one command pays to rewrite its whole
context when it resumes. This refines the [epic](/quest/m0/ci/README.md)'s
2026-10-25 decision that wall clock for its own sake is a non-goal: the bound
comes from the agent cache, and CPU time stays the measure for everything else.

Decided 2026-10-29 (user): time queued in `quest gate` admission counts.
kixelated/quest redesigns the gate so admission no longer outlasts a worker's
cache; this quest owns the gated commands' own run time.

Baseline, 2026-10-22 to 10-29: successful agent-run gates on the desktop, from
Claude Code transcripts. Times include admission wait and load from concurrent
agents, so this is an observation, not a controlled benchmark.

| Command | Runs | Median | p75 | Over 5 min |
| --- | ---: | ---: | ---: | ---: |
| `just check` | 1,961 | 0.9 min | 4.5 min | 24% |
| `just test` | 459 | 2.2 min | 8.5 min | 40% |

Every column leaves out the 177 check and 44 test runs that reached the
agent's 10-minute command limit, so the percentiles and shares over 5 minutes
understate the tail.

- First find which routes and steps exceed 5 minutes on representative diffs,
  separating admission wait, compile, and test time.
- Prefer the epic's levers, such as [slow tests](/quest/m0/ci/slow-tests.md),
  [hakari](/quest/m0/ci/hakari.md), and cache hits, and tighter diff selection,
  over moving coverage. Move a check to the queue or nightly only under the
  epic's rule: evidence and an explicit coverage tradeoff for each.
- Split any large single lever into its own quest.

## Related

- [Halve the slowest tests' CPU](/quest/m0/ci/slow-tests.md) - the largest test-time lever
- [Land main through quest land](/quest/m0/ci/land.md) - the queue gate that keeps checks moved out of the local gate
