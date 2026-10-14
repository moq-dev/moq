# CI cost and latency

## Goal

Pull requests get a CI verdict without queueing behind the hosted runner
limits, and every job spends its minutes on the work its diff selected rather
than on setup, cold caches, or repeated builds. Coverage does not shrink: work
moves to the merge queue, to nightly, or to a shared artifact, and anything
dropped from a pull request still gates the merge or runs nightly.

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

`cache.yml` has not published since 2026-10-11: its save step only runs when
the warm's `just ci check --all` and `just ci test --all` pass, and a
formatting slip and a flaky test on `main` blocked it. OBS on a `main` push
also saved into the same shared key space, so PRs restored its sliver of the
workspace. #5428 lets the warm save regardless and stops OBS saving; a
compile-only warm can follow if the warm's runtime matters.

Decisions (maintainer, 2026-10-13):

- macOS and Windows run in the merge queue, not on every PR push. The fast
  platform test suites move with them; the rest of Nightly stays nightly.
- Caches move to Cloudflare R2: mbx's S3 backend for Rust, and `nix.moq.dev`
  replacing Cachix for Nix. The infra lives in `infra/` beside apt and rpm,
  checked in where wrangler allows; one-time commands where it does not.
- Untrusted runs only read. Write credentials live in GitHub Environments
  whose deployment policy allows only `main` (plus release tags for the Nix
  signing key), so a pull request job cannot read them even from the same
  repository. Fork PRs get no secrets
  and run uncached.
- Self-hosted CI is dropped: hosted runners plus the R2 caches replace the
  spare-desktop runner, its cache server, and the cache release it waited on.

Measure each change against the numbers above and record the result in its PR.

## Required

- [Require platform gates](/quest/m0/ci/platform-queue.md) - activate the consolidated platform checks in the main and release rulesets after the workflow lands
- [mbx on R2](/quest/m0/ci/mbx-r2.md) - Rust builds on every OS read an R2 cache that only `main` writes, escaping the 10 GB Actions cache
- [Nix cache](/quest/m0/ci/nix-cache.md) - `nix.moq.dev` on R2 replaces Cachix for CI and releases
- [FFI artifact reuse](/quest/m0/ci/ffi-reuse.md) - identical FFI release builds are built once and shared across the language packages
