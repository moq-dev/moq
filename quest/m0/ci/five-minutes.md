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

Measured 2026-10-29 from 145 dispatch reports in agent transcripts since
10-15 (`check: seconds per module`): `check rs` is the only module whose
median exceeds 5 minutes (559 s median, 1,126 s p75, 69 of 104 runs over).
`test rs` follows (145 s median, 184 s p75); every other module's p75 is under
3 minutes. moq-net is the crate diffs touch most (39 of the last 200 commits on
`main`, 40% of those touching Rust), and its dependents span most of the
workspace, so a moq-net edit is the representative slow route.

One moq-net edit's `rs lint-changed` on the desktop, warm mbx cache, load near
77 on 24 cores: 1,880 s. Admission waited 267 s; `feature-shapes` linted every
dependent at both feature extremes in 981 s (52%); tokio-features and
media-features, selected only as dependents, took 294 s; `cargo doc` 159 s;
the release loom compile 112 s.

Landed in [moq-dev/moq#7282](https://github.com/moq-dev/moq/pull/7282): the
local `just check` runs `feature-shapes` and the net, QUIC, tokio, and media
feature contracts for the crates the diff changed, while the landing worker's `just ci`
keeps their dependents; the gate compiles the loom models in the dev profile
(46 s against release's 146 s after a moq-net edit). The same lint then took
319 s at load near 30, with `feature-shapes` at 29 s. The whole `just check`
for a moq-net edit took 547 s with load falling from 24 to 8: `check rs`
230 s and `test rs` 317 s, run one after the other in the rs lane. The test
phase is a 206 s build of moq-net's dependents and a 110 s run of 9,049 tests.

What remains for a moq-net edit, largest first:

- The test build recompiles every dependent after a moq-net edit. Candidates
  are the epic's [thin generics](/quest/m0/ci/btree-keys.md) and
  [hakari](/quest/m0/ci/hakari.md) levers, or running the rs lint beside the
  test build in a second target directory, which trades CPU for wall time
  against the epic's CPU measure. Decided 2026-10-30 (user): defer that
  parallel lint; measure it after the slow-tests and hakari quests land.
- The test run is the [slow tests](/quest/m0/ci/slow-tests.md) quest's.
- The lint's remaining 230 s: the dev loom compile and `cargo doc`. Decided
  2026-10-31 (maintainer, see the [epic](/quest/m0/ci/README.md)): the gate
  drops the loom compile, the Send-depth check (now its own Nightly job), the
  feature shapes, and the feature contracts (net-features's 62 s among them),
  and tests the relay's io_uring runtime in the one test build instead of a
  separate clippy pass.
- Measure the other routes the same way: a moq-ffi edit, which selects every
  binding's lane under `CHECK_JOBS=3`, and a JS-only edit.
- `just test` has no root recipe: bare `just test` lists the cross-language
  suites. Name which command the `just test` baseline above counts before
  measuring it.
- Prefer the epic's levers, such as [slow tests](/quest/m0/ci/slow-tests.md),
  [hakari](/quest/m0/ci/hakari.md), and cache hits, and tighter diff selection,
  over moving coverage. Move a check to the queue or nightly only under the
  epic's rule: evidence and an explicit coverage tradeoff for each.
- Split any large single lever into its own quest.

## Related

- [Halve the slowest tests' CPU](/quest/m0/ci/slow-tests.md) - the largest test-time lever
- [Unify dependency features](/quest/m0/ci/hakari.md) - cache hits across feature shapes
- [Land main through quest land](/quest/m0/ci/land.md) - the queue gate that keeps checks moved out of the local gate
