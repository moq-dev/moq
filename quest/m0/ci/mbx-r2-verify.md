# [S] Verify the R2 Rust cache

## Goal

Confirm the cache earns its keep for the hosted Nightly jobs on `main` that
still use R2 (Linux, macOS and Windows), and record the numbers in the PR
that closes this:

- Nightly's Linux, macOS and Windows jobs hit what the previous Nightly
  wrote.
- Restore plus build time against the 150-210s Actions restore of the ~6 GB
  store it replaced, from the same commit before and after.
- Each object expires a fixed time after upload (7 days since 2026-11-12), so a crate
  no hosted job builds in that window stays cold for the next hosted job. Count how often a Nightly
  job pays that rebuild, including when `Cargo.lock` changed more than 8 times
  since the previous Nightly (see Plan).
- Bucket size and request counts after a month with capped manifests
  (mbx 1.28.0 and later, from about 2026-11-26), against the expiry in
  `infra/mbx/lifecycle.json`.

Use scheduled Nightly runs on `main` only (`gh run list --workflow
nightly.yml --branch main --event schedule`), never a dispatch on a branch.

## Plan

Decided 2026-11-12 (maintainer, coordinator session): ✅ bump mbx to 1.30.0
first, then file the Windows misses upstream with 1.30.0 evidence (rejected:
file now on 1.29.2). 1.30.0 adds `mbx analyze` and `mbx stats` explanations
of why a compilation missed, and
[jdx/mr-boxington#903](https://github.com/jdx/mr-boxington/pull/903) fixes
local store eviction on Windows. The Windows platform job now runs
`mbx analyze` after its workspace build. It compares only against builds
recorded on the same runner, so on a fresh hosted runner a cross-run miss
likely reads as "misses with no earlier recording"; the report still names
the remaining causes and is what an upstream issue should carry.

Decided 2026-11-07 (maintainer, /quest-unblock): agents collect the R2
numbers with this desktop's wrangler login, and the maintainer only picks the
lifecycle. Baseline that day: 720,249 objects / 1.2 TB, with 18,006,968
`GetObject` and 4,892,769 `PutObject` requests from 2026-10-16 to 2026-11-07.

Decided 2026-11-07 (maintainer, /quest-unblock): ✅ shrink the expiry to 14
days, applied that day with `just infra mbx lifecycle`. At R2 list prices the
30-day window was headed for about 1.65 TB (about $25 a month in storage alone), and 14 days halves
that. Not chosen: keeping 30 days until the 2026-11-26 numbers;
lengthening it.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the release pull request
check moved to [m3](/quest/m3/mbx-r2-release-cache.md) (m2 before the milestone re-cut) behind the cut, since
`release` gets the platform Rust cache only when the cut merges `main` into
it; the Nightly checks stay here. Not chosen: moving this whole quest; merging
`main` into `release` now, which reverses the 2026-11-04 decision.

Decided 2026-10-22 (maintainer): narrowed to hosted jobs. Since
[#6040](https://github.com/moq-dev/moq/pull/6040) the merge queue runs on the
self-hosted `moq-queue` runner with mbx's local store, so it neither reads nor
writes R2.

Decided 2026-10-27 (maintainer): the cross-job miss report moved to
the cargo-hakari quest, which recorded its own before and after baseline and
finished 2026-11-06.

The first scheduled Nightly on mbx 1.28.0,
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673)
(2026-10-27), settled the bump: every hosted job reports `configured
read-write, effective read-write`, and macOS refused only a manifest already
oversized before it. Six such manifests stay unreadable until the
lifecycle deletes them, typically within a day of the 7-day rule applied 2026-11-12: `a90076a4...`,
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
  [#870](https://github.com/jdx/mr-boxington/pull/870), and
  `.github/actions/rust-cache/action.yml` pinned 1.29.2 (2026-11-03) from
  [#7653](https://github.com/moq-dev/moq/pull/7653) until the 1.30.0 bump
  below. The fix also offers
  legacy CRLF identities, so an in-window manifest from 1.28.0 remains usable.
  Verified 2026-11-05: the scheduled
  [Windows job](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127326015447)
  of run 42050683248 set up mbx 1.29.2, and its workspace build, against a
  manifest of 2,604 predictions, reported 846 hits, 224 misses, 168 not looked
  up and 1,019 prefetched. Its `Cargo.lock` differed from the previous
  scheduled run's, so those hits point to an inherited manifest, but the
  summary does not name the manifest identity it looked up; record that
  identity from a later Windows job before relying on inheritance.
- macOS: on 2026-10-28 the build did inherit, prefetching 732 predictions,
  yet hit only 13, with 1565 compilations not looked up. The install order is
  not the cause. Find out why the inherited predictions mostly miss, for
  example the lockfile changes or the expiry (30 days until 2026-11-07, 14
  days afterward, then 7 since 2026-11-12).
- Linux: Nightly's Linux jobs check out at depth 1, so mbx finds no earlier
  lockfile and reuses only a manifest written for the identical `Cargo.lock`.
  The doctest job hit 0 on 2026-10-24 and 2026-10-27, 115 on 2026-10-25, and
  46 on 2026-10-26. Decided 2026-10-29 (maintainer): give those jobs full
  history (`fetch-depth: 0` with `filter: blob:none`) only once Windows hits
  after the mbx bump, which shows inheritance pays. Windows hit on
  2026-11-05 (above), so this quest's PR gives Nightly's five Linux
  Rust-cache jobs full history. On the first scheduled Nightly after it
  lands, compare each job's hits with its own depth-1 hits: the doctest
  job's with the baseline below, and each other job's with its last
  completed depth-1 scheduled run, since the jobs build different workloads.
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
neither platform failure is evidence against the mbx bump.

The [Linux doctest job](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408205232)
also reported effective read-write, but 0 hits, 913 not looked up, 51 bypassed
and 1 remote failure, despite loading 756 predictions. Its Cargo test build
took 6m 16s; the cache summary reported 1.1 GiB downloaded and 2.6 GiB
uploaded. The [previous scheduled run](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517732)
had 0 hits, 878 not looked up, 50 bypassed and 3 remote failures, with a
6m 26s Cargo build. These are different commits, not the same-commit
restore-plus-build comparison the Goal requires. There were 13 `Cargo.lock`
changes between these two runs. Record such intervals for the full-history
platform jobs' 8-state lookback, not as the cause of these Linux misses:
Linux has no history to search even within that window, and the loaded
predictions did not match. The current run also follows the workspace-hack
feature unification, so changed compiler inputs remain a separate cause.

Evidence checked 2026-11-08, scheduled run
[42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545)
(`Cargo.lock` changed 5 times since each of the previous three scheduled
runs, so every platform stayed within the 8-state lookback):

- [macOS](https://github.com/moq-dev/moq/actions/runs/42487375545/job/128752177964)
  workspace build: 567 hits, 15 misses, 687 not looked up, 1,778 prefetched,
  up from 13 hits on 2026-11-02. Inheritance now pays on macOS; the
  remaining question is the 687 not looked up.
- [Windows](https://github.com/moq-dev/moq/actions/runs/42487375545/job/128752177860)
  workspace build: 163 hits, 757 misses, 310 not looked up, 272 prefetched,
  down from 846 hits on 2026-11-05 at a similar lockfile distance. The
  summary still names no manifest identity, so whether it inherited the
  previous Nightly's manifest or an older one is unknown.
- [Linux doctest](https://github.com/moq-dev/moq/actions/runs/42487375545/job/128752096274)
  (depth 1): 174 hits, 279 misses, 305 not looked up, 414 prefetched,
  4 remote failures.

Evidence checked 2026-11-12, scheduled runs
[42659882487](https://github.com/moq-dev/moq/actions/runs/42659882487) (11-09),
[42842827718](https://github.com/moq-dev/moq/actions/runs/42842827718) (11-10) and
[43041909115](https://github.com/moq-dev/moq/actions/runs/43041909115) (11-11).
[#8342](https://github.com/moq-dev/moq/pull/8342), which gives Linux full
history, merged 2026-11-12 07:19 UTC, after all three, so their Linux jobs
are still depth-1 and serve only as baselines. `Cargo.lock` changed 5, 1 and
1 times since each previous scheduled run, all within the 8-state lookback.
Workspace builds (hits / misses / not looked up):

- macOS: 753 / 58 / 401, 964 / 227 / 4, 1,008 / 182 / 0. Inheritance now
  predicts nearly every compilation, which settles the 687 not looked up
  from 11-08; macOS reuses earlier Nightlies.
- Windows: 123 / 636 / 415, 108 / 1,049 / 18, 112 / 1,043 / 14. Like macOS,
  the predictions now cover the build, but about 90% of looked-up keys miss,
  even with one lockfile change since a Nightly that uploaded 1.2 GiB from
  the same job. Inheritance works, but the cause of the misses is
  unconfirmed: the keys may differ between builds, the previous job may not
  have stored them, they may have expired, or the build may have loaded
  another manifest. The summary still names no manifest identity, so
  comparing the loaded manifest with the stored keys would tell these
  apart. Down from 846 hits on 2026-11-05.
- Linux depth-1 baselines, hits only per run (not the hits / misses / not
  looked up triple above): doctest 124, 502 (0 misses), 150; Media (firefox
  player) 58, 84 (11-11 cancelled before its build); Audio quality, hits of
  each of its three builds per night, 74, 77, 0 (11-09), 57, 97, 0 (11-10),
  54, 25, 0 (11-11). The doctest job is one entry of the `Tests` matrix.
  The remaining Linux Rust-cache jobs (the `Nightly` matrix, the `Tests`
  matrix's other recipes, and `Shipping sizes`) have no baseline recorded
  here; read each one's depth-1 baseline from its own cache summary in these
  three runs.

## Waiting

- GitHub Actions: the first scheduled Nightly on mbx 1.30.0 shows whether
  Windows still misses most looked-up keys (above). If it does, file an issue
  on jdx/mr-boxington with that run's `Explain Rust cache misses` output and
  cache summary, following its CONTRIBUTING.md and AGENTS.md AI disclosure
  rules (the maintainer approved filing). If the misses are gone, record that
  run's numbers here. Either way, drop this item.
- GitHub Actions: the first scheduled Nightly after #8342 (2026-11-12 afternoon or
  later) must show each Linux Rust-cache job's hits against its depth-1
  baseline (above, or for the `Nightly` matrix, the `Tests` matrix's other
  recipes and `Shipping sizes`, their own summaries in runs 42659882487, 42842827718 and 43041909115) and the
  174-hit doctest run of 11-08, and scheduled runs
  must supply the same-commit restore-plus-build comparison and the
  rebuild-frequency count in the Goal.
- moq-dev/moq: no earlier than 2026-11-26, an agent on the maintainer's
  desktop collects `mbx-moq-dev`'s size and request counts for the first
  month and checks that the six old oversized manifests expired. Size comes
  from `npx wrangler r2 bucket info mbx-moq-dev`. Request counts come from
  Cloudflare's GraphQL API (`r2OperationsAdaptiveGroups`, filtered on
  `bucketName` and grouped by `actionType`), authenticated with the
  `oauth_token` in `~/.config/.wrangler/config/default.toml`. Record them
  here beside the 2026-11-07 baseline, split at each lifecycle change (that
  date's, and 2026-11-12's 7-day rule),
  and count the Nightly jobs that rebuilt a crate the expiry deleted, listing as unknown any it cannot attribute. This count waits until the 7-day rule has run for at least 7 days,
  so the shorter window has data. Bring the lifecycle back to the
  maintainer only if that count argues for lengthening it.

## Related

- [The Rust cache stops paying for uploads R2 refuses](/quest/m0/ci/mbx-failed-puts.md) - measures the same bucket's requests and also reports to mbx upstream
