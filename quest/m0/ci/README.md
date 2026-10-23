# CI cost and latency

## Goal

Pull requests are validated locally with `just ci` and get their cloud verdict
from the merge queue without waiting behind the hosted runner limits, and
every job spends its minutes on the work its diff selected rather
than on setup, cold caches, or repeated builds. Concurrent local agents share
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
  was optimized. Decided 2026-10-22 (maintainer): the queue runner is the
  persistent `nix/runner` VM (no host mounts or tailnet, firewalled to public
  egress), in use since 2026-10-20. Over 32 one-pass entries it took a median
  5.5 minutes per merged PR (p90 11.9, at most 24.2) against 21 minutes on a
  hosted runner, with no per-minute bill. Blacksmith is not pursued.

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
wall time per PR run.

Measure each change against a current baseline and record the result in its PR.
The historical samples locate candidates; they are not controlled benchmarks.

## Required

- [Require the release gate](/quest/m0/ci/release-gates.md) - condition: the next cut carries `release-gate.yml`; then release PRs require it
- [Share local check capacity](/quest/m0/ci/local-gate.md) - gate expensive phases and bound internal fanout
- [Bump to the manifest fix](/quest/m0/ci/mbx-bump.md) - condition: an mbx release contains jdx/mr-boxington#798; then bump the Rust cache's pin
- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - measure the hosted Nightly jobs' hits, restore time, and expiry once the bump lands
- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - confirm CI and releases use `nix.moq.dev` and retire Cachix
- [The gate builds a Nix package](/quest/m0/ci/nix-package-gate.md) - a change that breaks `nix build .#moq-relay` or the Dockerfile fails before `main`
- [First shared FFI release](/quest/m0/ci/nightly-ffi-reuse.md) - condition: the first `moq-ffi-v*` tag that runs `Release FFI`, and a `Shipping sizes` run; verify the wrappers and size keys
