# [L] Gate each diff in under five minutes

## Goal

`just check`, including its selected modules' test phases, finishes in under
5 minutes of wall time for every diff-scoped run on the desktop with warm
caches, counting time queued in `quest gate` admission. A worker agent then runs its own gate inside its
5-minute prompt-cache window. Explicit full runs (`--all`, nightly, release)
are exempt; work moved out of the diff-scoped gate goes to the merge queue or
nightly with its coverage kept.

## Plan

Decided 2026-11-03 (maintainer): the bound covers "`just check`, including its
selected modules' test phases" ✅, not bare `just test` or a separately named
set of cross-language suites. `just check` is the diff-scoped gate and already
runs each selected module's tests; bare `just test` only lists the
cross-language suites. The historical `just test` baseline row below is not
compared with new measurements.

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

The first reduction landed in [moq-dev/moq#7282](https://github.com/moq-dev/moq/pull/7282): the
local `just check` ran `feature-shapes` and the net, QUIC, tokio, and media
feature contracts for the crates the diff changed, while the landing worker's `just ci`
kept their dependents; the gate compiled the loom models in the dev profile
(46 s against release's 146 s after a moq-net edit). The same lint then took
319 s at load near 30, with `feature-shapes` at 29 s. The whole `just check`
for a moq-net edit took 547 s with load falling from 24 to 8: `check rs`
230 s and `test rs` 317 s, run one after the other in the rs lane. The test
phase is a 206 s build of moq-net's dependents and a 110 s run of 9,049 tests.

The settled gate changes have since landed:

- [moq-dev/moq#7472](https://github.com/moq-dev/moq/pull/7472) removed the
  feature shapes and contracts from the shared local and landing gate. The
  Features workflow retains the matrix nightly and on release PRs. The
  relay's io_uring tests join the default test build on Linux.
- [moq-dev/moq#7473](https://github.com/moq-dev/moq/pull/7473) removed the loom
  compile and moved Send-depth to its own Nightly job.

Measured 2026-11-02 on the desktop (x86_64, Rust 1.98.1, Bun 1.4.2,
mbx 1.25.0), with other agents running. Each row is one observation after a
successful preparation run, not a controlled comparison with the historical
samples or proof of the bound for every diff. A comment-only source edit
invalidated the package again after preparation. The temporary edits were
restored after measuring.

`bench/local.py --checkout . --workers 4 --jobs 3 --samples 1 --cache ... --
just check HEAD` admits the whole command once, separates admission from work,
and records child CPU time. `HEAD` makes the edited source the entire measured
diff. The net and JS runs used
[196e4f8126](https://github.com/moq-dev/moq/commit/196e4f81262f71d1477c7b2c5e14e0e241c12125);
the FFI run used
[e6afdcca32](https://github.com/moq-dev/moq/commit/e6afdcca3210962b7ee34896fc0cff87cfeec186)
with the installed pkg-config lookup fix. Logs remain in the checkout's
`.tmp/local-bench/`.

| Edited source | Admission (s) | Work (s) | Total (s) | CPU (s) | Starting load (1 min) |
| --- | ---: | ---: | ---: | ---: | ---: |
| `rs/moq-net/src/lib.rs` | 88.2 | 227.1 | 315.3 | 1,048.0 | 31.4 |
| `js/net/src/index.ts` | 0.2 | 228.9 | 229.0 | 352.4 | 12.5 |
| `rs/moq-ffi/src/lib.rs`, explicit Gradle executable | 0.2 | 101.1 | 101.2 | 219.8 | 9.7 |

The net lane was 34 s of lint and docs, then 193 s of tests: a 68 s build
and 122 s running 9,529 tests (all passed). The JS lane was 115 s checking,
54 s testing, then the shared Markdown and tooling checks. The FFI route ran
the Rust, Python, C, C++, Kotlin, Go, OBS compile, and Dart lanes with
`CHECK_JOBS=3`; Swift skipped because this Linux host has no Swift toolchain.

The first FFI attempt exposed two base-branch failures:

- C and C++ installed and ran their CMake probes, then failed to find their
  pkg-config packages. Nix's pkg-config wrapper replaces `PKG_CONFIG_PATH`
  with its target-specific value. Passing the installed `.pc` file directly
  fixes the lookup without changing the wrapper's dependency search. The
  same explicit-prefix queries in OBS and Nix packaging now use their `.pc`
  file too. Published smoke keeps its name-based lookup to test consumer
  resolution. C and C++ passed in both subsequent
  configured FFI runs.
- Kotlin recursively called its own Gradle wrapper because `command -v gradle`
  selected Nix's exported function. The attempt was cancelled and its child
  processes stopped. [moq-dev/moq#7632](https://github.com/moq-dev/moq/pull/7632)
  already owns this fix. Both subsequent FFI runs used the supported
  `GRADLE_CMD` override set to the dev shell's absolute Gradle executable;
  the table is conditional on that configuration, not a passing default run.

Final local validation also reproduced a namespace-probe race: a raw path
flake copied ignored CMake output while another lane replaced a generated
`render` file. The probe now reads `flake.lock` and fetches its pinned nixpkgs
directly, without copying the checkout. It still reports the host builder's
namespace failure, allowing the existing local skip; strict CI continues to
require a capable builder.

What remains:

- Admission still makes the measured net route exceed 5 minutes despite its
  command taking 227 s. The 2026-10-29 admission decision above remains with
  moq-dev/quest.
- Repeat the default FFI route without the override; the Gradle fix
  ([moq-dev/moq#7632](https://github.com/moq-dev/moq/pull/7632)) merged
  2026-11-04.
- Keep the dependency-build and cache work with
  [hakari](/quest/m0/ci/hakari.md). Decided 2026-10-30 (user): defer parallel
  Rust lint until that quest lands, then measure it. The slow-tests reduction
  landed in [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987); the
  BTree thin-generics lever measured about 30 ms of CPU per build and was dropped.
- Prefer the epic's levers and tighter diff selection over moving coverage.
  Moving a check to the queue or nightly still needs evidence and an explicit
  coverage tradeoff. Split a large independent lever into its own quest.

## Waiting

- moq-dev/quest: admission must keep the whole gate inside the worker's cache
  window; repeat the net measurement after its admission work lands.

## Related

- [Unify dependency features](/quest/m0/ci/hakari.md) - cache hits across feature shapes
