# CI cost and latency

## Goal

Pull requests are validated locally with `just ci` and get their cloud verdict
from the [batched gate](/quest/m0/ci/land.md) without waiting behind the
hosted runner limits, and every job spends its minutes on the work its diff
selected rather than on setup, cold caches, or repeated builds. Concurrent local agents share
expensive-command capacity, and retained tests earn their cost through useful
regression signal. Valuable checks stay gated or move to an appropriate queue
or nightly run; measured low-value redundancy may be retired.

## Plan

Measured 2026-10-13 from the org billing API (Oct 1-13, all moq) and a 1-in-4
sample of job timings:

| Runner | Minutes | List price |
|---|---|---|
| macOS | 33k | $2,066 |
| ARM Linux | 98k | $492 |
| Windows | 34k | $344 |
| x86 Linux | 30k | $178 |

Public repositories are not billed, but the org is capped at 60 concurrent
hosted jobs and 5 macOS jobs (Team plan), and that macOS cap is the queue.
In the sample, macOS spend came from Platform on PRs (about a third),
OBS (a quarter), Nightly's release tarballs, Swift, and the Release Dart FFI
dry run on PRs (five macOS builds per run). On Linux the PR Check and Test
jobs and the `cache.yml` warm dominate. A branch's PR Check runs a median 2
and a p90 5 times before it merges.

Decisions (maintainer, 2026-10-13):

- macOS and Windows run in the merge queue, not on every PR push. The fast
  platform test suites move with them; the rest of Nightly stays nightly.
  Superseded 2026-10-19: they run nightly on `main` and are required on
  `release`.
- Caches move to Cloudflare R2: mbx's S3 backend for Rust, and `nix.moq.dev`
  replacing Cachix for Nix. The infra lives in `infra/` beside apt and rpm,
  checked in where wrangler allows; one-time commands where it does not.
- Untrusted runs only read. Write credentials live in GitHub Environments
  whose deployment policy allows only `main` (plus release tags for the Nix
  signing key), so a pull request job cannot read them even from the same
  repository. Fork PRs get no secrets
  and run uncached.
- Decided 2026-10-19: the merge queue is trusted like `main`, since a queued
  PR is about to land. Rust cache writers are the jobs on `main` and in its
  queue, and no separate warm rebuilds what landed. Since
  [#6040](https://github.com/moq-dev/moq/pull/6040) the queue runs on the
  self-hosted `moq-queue` runner with mbx's local store, so the R2 writers are
  the hosted Nightly jobs on `main`.
- Self-hosted CI is dropped: hosted runners plus the R2 caches replace the
  spare-desktop runner, its cache server, and the cache release it waited on.
  Reopened 2026-10-19, conditionally, to compare runners once the pipeline
  was optimized. Decided 2026-10-22 (maintainer): the queue runs on a
  persistent self-hosted runner, in use since 2026-10-20. Over 32 one-pass
  entries it took a median 5.5 minutes per merged PR (p90 11.9, at most 24.2)
  against 21 minutes on a hosted runner, with no per-minute bill. Blacksmith
  is not pursued.
- Decided 2026-10-25 (maintainer), superseding the 2026-10-22 choice of the
  `nix/runner` VM: no VM. The queue runner is the deployed systemd-nspawn
  container `container@moq-ci`, and new queue runners go into it. Reason: the
  CI host should run mostly sandboxed through kernel primitives (namespaces,
  cgroups and slices, seccomp), which the container already uses, so a VM is
  not needed for isolation. The `nix/runner` VM module is not the deployment
  target.

Decided 2026-10-26 (maintainer), from 414 queue merges over 2026-10-23 to
10-26: the queue's latency is serial waiting (98%), not GitHub's overhead
(about 1 minute per merge), with a median of 18 entries ahead and 12.7% of
runs failing. GitHub's merge queue cannot test several PRs in one run, so a
batched gate replaces it: `quest land`, shared through moq-dev/quest, lands
a tested chain of squash commits ([land](/quest/m0/ci/land.md)). Decided
2026-10-30 (maintainer): it runs as a daemon on the maintainer's host, not as
a workflow on the self-hosted runner. This supersedes concurrent queue
entries on a second runner. Batching gains most when failures are rare, so the
[flake epic](/quest/m0/test-flakes-2/README.md) moved to m0 and
`MIGRATE.md` Unreleased sections, a top source of conflicts, stop being
written per PR.

Decided 2026-10-23 (maintainer), to speed up the serial queue: structural
changes first (concurrent entries, parallel modules, one test build), then a
measured nightly-compiler experiment.

Decided 2026-10-15: local CPU admission, test-cost reductions, and setup
reductions are all m0. `quest gate` owns generic command admission; repositories
adopt it through their entrypoints. Agents remain concurrent. Selective test
retirement is allowed with evidence and an explicit coverage tradeoff, replacing
the earlier blanket no-shrink constraint. Check/Test remain on PRs and the queue
(superseded 2026-10-19, below).

Decided 2026-10-19, to cut review-to-merge latency and CI spend: cloud CI runs
only in the merge queue, and contributors and agents run `just ci` locally
before pushing. The queue's Linux gate becomes one job once Check sheds its
feature permutations and release-profile builds. Expensive and optional checks
run nightly on `main` and are required on `release`. Faster runners come last,
chosen from measurements of the optimized pipeline. Each CI pass costs twice
per merged PR today (PR and queue), at 13 minutes median and 26 minutes p90 of
wall time per PR run. Superseded in part 2026-10-24: the PR gate checks
feature shapes for the crates a branch changes; Nightly keeps the full matrix.

Decided 2026-10-25 (maintainer): across moq's many CI and release builds
(targets, features, FFI languages, Nix packages), each crate configuration
compiles once and is reused through the cache, and each run builds and tests
only what its diff can affect, measured in runner CPU time. Wall clock for its
own sake and replacing cargo are non-goals. mbx already caches each compilation,
so the work is making builds identical enough to hit it (flags in
`.cargo/config.toml`, cargo-hakari), and auditing the package selection `just ci`
already does. Nix builds run sandboxed and cannot reach R2, so they stay on crane
for packaging only; CI builds with cargo and mbx. Decided 2026-10-26 from
moq-dev/rust2's [Nix build benchmark](https://github.com/moq-dev/rust2/pull/63),
in billions of instructions over a clean build, a second package, the moq-net edit,
and a lockfile bump: cargo with mbx totals 9,692 and crane 14,756. crate2nix built
to the release profile totals 14,538: it wins only the lockfile bump (1,255 against
crane's 3,681) and costs 11-27% more on the rest.

Decided 2026-10-25 (maintainer), from a profile of moq-net: monomorphized
generics dominate its build (53% of the library's CPU), and its test target
costs 3x the library. A "thin generics" profile shrinks them: generic entry
points delegate to non-generic bodies, and BTreeMap instantiations are shared.
moq-dev/rust2's driver lints the rule later. Making IETF support a cargo
feature was considered and not planned: the churn outweighs the build saving.

Measure each change against a current baseline and record the result in its PR.
The historical samples locate candidates; they are not controlled benchmarks.

## Required

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - write secrets and publisher identities reach only protected branches and tags, never a pushed branch
- [Land main through quest land](/quest/m0/ci/land.md) - a host daemon tests small batches of queued PRs once and lands each as its tested squash commit
- [Gate each diff in under five minutes](/quest/m0/ci/five-minutes.md) - `just check` and `just test` each finish within a worker's 5-minute prompt cache, admission wait included
- [Measure the parallel frontend](/quest/m0/ci/nightly-levers.md) - a pinned nightly's parallel frontend, adopted only if the numbers beat its risks; Cranelift is ruled out
- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - confirm the hosted Nightly jobs reuse the previous Nightly, then measure restore time and expiry
- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - confirm releases use `nix.moq.dev`, check its bucket sizes, and retire Cachix
- [Unify dependency features](/quest/m0/ci/hakari.md) - a cargo-hakari workspace-hack so every job compiles each dependency alike and mbx hits across jobs
- [Fewer BTreeMap instantiations](/quest/m0/ci/btree-keys.md) - moq-net shares BTreeMap key and value types, kept only if its CPU time drops
- [Halve the slowest tests' CPU](/quest/m0/ci/slow-tests.md) - the slowest 1% of tests take under half their 289 CPU-seconds, with coverage kept
- [Gate send-depth](/quest/m0/ci/send-depth-gate.md) - the merge gate runs send-depth on a pinned nightly when moq-net changes, after #6890 fixes main
- [Cargo aliases in nested checkouts](/quest/m0/ci/cargo-alias-strings.md) - `cargo build-ffi` and `cargo moq` work from a worktree nested inside another checkout
- [Release credentials off main](/quest/m0/ci/publish-release-only.md) - `publish` admits only `release` and release tags once its `workflow_run` publishers run on their tag
- [Biome checks every file it can parse](/quest/m0/ci/biome-whole-repo.md) - Biome runs over the whole repository and main is clean under it
- [workspace.test.py ignores the caller's gate filter](/quest/m0/ci/workspace-test-env.md) - the dispatcher test passes under any `just gate` module filter
- [PRs build the feature shapes they can break](/quest/m0/ci/pr-feature-builds.md) - a PR touching a crate's source runs that crate's isolated feature builds
