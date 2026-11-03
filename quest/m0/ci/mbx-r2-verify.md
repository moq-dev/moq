# [S] Verify the R2 Rust cache

## Goal

Confirm the cache earns its keep for the hosted jobs that still use R2
(Nightly on `main`, release pull requests, and the macOS and Windows jobs),
and record the numbers in the PR that closes this:

- A release pull request's `Check the cache` step reports `effective
  read-only`. Condition: `release` carries the Rust cache, which its
  `platform.yml` still lacks on 2026-10-28; check with
  `git show origin/release:.github/workflows/platform.yml | grep rust-cache`.
- Nightly's Linux, macOS and Windows jobs hit what the previous Nightly
  wrote.
- Restore plus build time against the 150-210s Actions restore of the ~6 GB
  store it replaced, from the same commit before and after.
- Each object expires 30 days after upload, so a crate no Nightly builds in
  that window stays cold for the next hosted job. Count how often a Nightly
  job pays that rebuild, including when `Cargo.lock` changed more than 8 times
  since the previous Nightly (see Plan).
- Bucket size and request counts after a month with capped manifests
  (mbx 1.28.0 and later, from about 2026-11-26), against the 30-day expiry
  in `infra/mbx/lifecycle.json`.
  Shorten or lengthen it from that.

Use scheduled Nightly runs on `main` only (`gh run list --workflow
nightly.yml --branch main --event schedule`), never a dispatch on a branch.

## Plan

Decided 2026-10-22 (maintainer): narrowed to hosted jobs. Since
[#6040](https://github.com/moq-dev/moq/pull/6040) the merge queue runs on the
self-hosted `moq-queue` runner with mbx's local store, so it neither reads nor
writes R2.

Decided 2026-10-27 (maintainer): the cross-job miss report moved to
[unifying dependency features](/quest/m0/ci/hakari.md), which measures its own
before and after baseline and no longer waits on this quest.

The first scheduled Nightly on mbx 1.28.0,
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673)
(2026-10-27), settled the bump: every hosted job reports `configured
read-write, effective read-write`, and macOS refused only a manifest already
oversized before it. Six such manifests stay unreadable until the 30-day
lifecycle deletes them, the last about 2026-11-26: `a90076a4...`,
`5cb8fb79...`, `b0310ce4...`, `18d136a6...` (17235558 bytes, first refused in
[40090959108](https://github.com/moq-dev/moq/actions/runs/40090959108)),
`2ed9df47...` and `0d4f4e3f...`. A refusal for any other digest means the
writer cap in mbx 1.28.0 did not hold.

No platform reuses the previous Nightly yet. mbx names a Cargo build's
prediction manifest by its workspace's `Cargo.lock` digest. When a lockfile
has no manifest yet, mbx inherits one from up to 8 earlier lockfile states,
found through Git history (`previous_build_identities` in `mbx-cache-cargo`,
`inherit_task_manifest` in `mbx-cache-core`). Without a prediction, a
compilation is "not looked up".

- Windows: 0 hits on 1.28.0 on both 2026-10-27 and
  [2026-10-28](https://github.com/moq-dev/moq/actions/runs/40698329026), each
  reporting that "a manifest predicting 124 compilations was loaded, but none
  matched this build". We reported it upstream as
  [jdx/mr-boxington#869](https://github.com/jdx/mr-boxington/issues/869),
  blaming the preceding `cargo install uniffi-bindgen-cpp`. The fix,
  [jdx/mr-boxington#870](https://github.com/jdx/mr-boxington/pull/870)
  (merged 2026-10-29, after v1.29.0), found a different cause. Git for
  Windows checks `Cargo.lock` out with CRLF, while history holds LF, so no
  earlier lockfile state ever matched. The build then fell back to the newest
  local manifest, which was the install's. Since 1.28.0 an install builds
  under its own workspace, so the install order does not matter; it stays
  where `main` has it. mbx v1.29.1 (2026-11-01) includes
  [#870](https://github.com/jdx/mr-boxington/pull/870), so this quest's PR bumps
  `.github/actions/rust-cache/action.yml` from 1.28.0 to 1.29.1. Check that the
  first scheduled Nightly after it lands loads a manifest of more than 124
  predictions and hits on Windows.
- macOS: on 2026-10-28 the build did inherit, prefetching 732 predictions,
  yet hit only 13, with 1565 compilations not looked up. The install order is
  not the cause. Find out why the inherited predictions mostly miss, for
  example the lockfile changes or the 30-day expiry.
- Linux: Nightly's Linux jobs check out at depth 1, so mbx finds no earlier
  lockfile and reuses only a manifest written for the identical `Cargo.lock`.
  The doctest job hit 0 on 2026-10-24 and 2026-10-27, 115 on 2026-10-25, and
  46 on 2026-10-26. Decided 2026-10-29 (maintainer): give those jobs full
  history (`fetch-depth: 0` with `filter: blob:none`) only once Windows hits
  after the mbx bump, which shows inheritance pays.
- `Cargo.lock` changed 7, 2, 6, 10, 8 and 8 times between consecutive
  scheduled Nightlies from 2026-10-21 to 2026-10-27. Nightly is the only
  hosted writer for Windows and macOS, so after more than 8 changes the
  previous Nightly is out of inheritance reach even with full history.
  Decided 2026-10-29 (maintainer): accept that, and count it under the
  rebuild item in the Goal before asking upstream for a deeper lookback.

Evidence checked 2026-11-02: the latest scheduled Nightly,
[41483654426](https://github.com/moq-dev/moq/actions/runs/41483654426), still
uses 1.28.0, so it cannot verify the Windows fix. Its
[Windows job](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408629378)
failed installing NASM from Chocolatey (HTTP 503), before setting up mbx.
Its [macOS job](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408628726)
reported effective read-write. The workspace check took 10m 25s with 13 hits,
1815 not looked up, no prefetch and no download; the preceding generator
install had 122 hits and 121 prefetched. Those install hits do not prove the
workspace reused the previous Nightly. The macOS job later failed tests;
neither platform failure is evidence against 1.29.1.

The [Linux doctest job](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408205232)
also reported effective read-write, but 0 hits, 913 not looked up, 51 bypassed
and 1 remote failure, despite loading 756 predictions. Its Cargo test build
took 6m 16s; the cache summary reported 1.1 GiB downloaded and 2.6 GiB
uploaded. The [previous scheduled run](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517732)
had 0 hits, 878 not looked up, 50 bypassed and 3 remote failures, with a
6m 26s Cargo build. These are different commits, not the same-commit
restore-plus-build comparison the Goal requires. There were 13 `Cargo.lock`
changes between these two runs, beyond the accepted 8-state inheritance
window; count this as an out-of-window rebuild opportunity, not proof that
expiry caused either miss. The current run also follows the workspace-hack
feature unification, so changed compiler inputs remain a separate cause.

## Waiting

- GitHub Actions: after this PR lands, the first scheduled Nightly on 1.29.1
  must demonstrate Windows workspace prediction inheritance and hits. Until
  then, keep Linux's shallow checkout as decided above. Do not dispatch Nightly.
- moq-dev/moq: `release` still lacks the platform Rust cache as checked on
  2026-11-02. Once it carries the cache, inspect a release pull request's
  `Check the cache` step for effective read-only.
- GitHub Actions: scheduled runs must supply the remaining cross-platform
  reuse, restore-plus-build comparison and rebuild-frequency evidence in the
  Goal. Current timings are not a matched-commit comparison, and macOS's
  inherited-prediction misses remain unexplained.
- Maintainer: from about 2026-11-26, collect R2 bucket size and request counts
  for the first month and check that the six old oversized manifests expired.
  Use the numbers to decide the lifecycle; retain 30 days until then.

## Related

- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - the same R2 and Environment bootstrap for Nix
