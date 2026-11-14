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

Decided 2026-11-13 (maintainer, /quest-complete): ✅ after the first
scheduled Nightly past [#8631](https://github.com/moq-dev/moq/pull/8631),
add the Windows numbers to
[jdx/mr-boxington#950](https://github.com/jdx/mr-boxington/discussions/950)
only if they add something, and file a new issue only if Windows still misses
most looked-up keys with no download-budget refusals (Waiting). #950 already
covers budget refusals and names the 10 GiB Windows budget, and jdx replied
there on 11-13 that prefetch and restores share one counter. Rejected:
filing the drafted issue now; posting nothing upstream.

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
  Rust-cache jobs full history; it landed as
  [#8342](https://github.com/moq-dev/moq/pull/8342). Each job's hits against
  its own depth-1 hits are under "Evidence checked 2026-11-13" below.
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
  Every other Linux job's baseline is in the 2026-11-13 table below.

Evidence checked 2026-11-13, scheduled runs
[43250182795](https://github.com/moq-dev/moq/actions/runs/43250182795)
(11-12 15:39 UTC, the first after #8342, still mbx 1.29.2) and
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(11-13, the first on mbx 1.30.0). Both predate
[#8631](https://github.com/moq-dev/moq/pull/8631) (11-13 15:36 UTC), which
lifts mbx's download budget from its hosted default (5 GiB on Linux, 10 GiB
on Windows) to 20 GiB. Numbers come from each job's `mbx[cache]: object
cache` lines.

- 11-13 is an expiry wave. `NoSuchKey` prefetch failures went from 20 per
  night (11-11, 11-12) to 5,270 across 38 jobs, including 2,837 on macOS,
  which never reaches its budget. The 7-day rule applied 11-12 deleted every
  object stored before about 11-06 at once: a refused 412 put does not
  refresh an object, so a unit every night restores still expires 7 days
  after its first upload. macOS's workspace build fell from
  [989 / 141 / 59](https://github.com/moq-dev/moq/actions/runs/43250182795/job/131296895948)
  on 11-12 to
  [650 / 556 / 1](https://github.com/moq-dev/moq/actions/runs/43447849289/job/131965078558)
  on 11-13, and every platform fell with it, so 11-13 cannot isolate either
  full history or mbx 1.30.0. For the rebuild count in the Goal: a unit that
  never changes still rebuilds once every 7 days, since no restore refreshes
  it, and this wave was larger because the switch from 14 to 7 days expired
  a week of uploads at once.
- Windows still misses most looked-up keys. The
  [11-13 workspace build](https://github.com/moq-dev/moq/actions/runs/43447849289/job/131965078875)
  had 92 hits, 1,062 misses, 13 not looked up and 237 prefetched, and
  downloaded exactly 10.0 GiB. 11-12 had 116 / 1,030 / 19, also at
  10.0 GiB. Its `Explain Rust cache misses` step (`mbx analyze`) puts all
  1,062 misses under "misses with no earlier recording", as expected on a
  fresh runner, so it names no cause. The download budget is the leading
  suspect. The only night with many hits,
  [846 on 11-05](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127326015447),
  downloaded 8.7 GiB with no budget refusals. Every night since 11-08 hit
  the 10 GiB cap (2,591 to 6,840 refusals) and got 92 to 163 hits. Linux,
  also budget-bound, logs 28,000 to 45,000 "action metadata is missing"
  prefetch warnings a night, and macOS logged none before the expiry wave.
- Linux full history works as intended for prediction: not looked up fell to
  near 0 in most jobs (doctest about 200 to 0, `rs quic-sim` 598 to
  33, `rs send-depth` 596 to 1). It did not raise hits, because every job
  still spends its 5 GiB budget on prefetch, so the new predictions mostly
  become misses. On 11-12, the cleanest comparison (one change, before the
  expiry wave), the summed hits of the jobs below were 10,990, inside the
  depth-1 range of 9,748 to 11,065. Ten jobs were above their own range,
  16 inside it and 3 below it (`gate` with 3 more builds than before,
  `test drill impaired` already falling from 406 to 259, and
  `rs rung-disable` 5 under). No job regressed because of full history. On
  11-13, 15 fell below their range and the sum fell to 9,317, which is the
  expiry wave above. Doctest: 165 on 11-12 and 78 on 11-13, against the
  174 of 11-08.

Linux hits per job (depth-1 nights 11-09, 11-10, 11-11; then 11-12 and
11-13 with full history). Jobs with no `mbx[cache]` summary build no Rust
through mbx: `Audit`, `JS audit`, `Python audit`,
`Rust coverage`, `JS coverage`, `JS benchmarks`, `BBB rendition
alignment`, `gh test-integration`, `test qmux`, `test drill-sensitivity`,
`js worklet-matrix`, `rs miri` and `test route-key-model`.

| Job | 11-09 | 11-10 | 11-11 | 11-12 | 11-13 |
| --- | --- | --- | --- | --- | --- |
| Tests: `rs doctest --workspace` | 124 | 502 | 150 | 165 | 78 |
| Tests: `gate` | 1,851 | 1,715 | 2,001 | 1,306 | 2,103 |
| Tests: `test resume-model` | 119 | 114 | 218 | 143 | 222 |
| Tests: `rs cli-export` | 155 | 142 | 140 | 181 | 72 |
| Tests: `test drill impaired` | 406 | 398 | 259 | 219 | 61 |
| Tests: `rs quic-sim` | 29 | 28 | 29 | 181 | 59 |
| Tests: `rs audio-devices` | 0 | 400 | 131 | 150 | 79 |
| Tests: `rs uring` | 78 | 75 | 42 | 248 | 318 |
| Tests: `rs hls-conformance` | 194 | 438 | 232 | 194 | 91 |
| Tests: `rs quic-stress` | 172 | 104 | 265 | 149 | 71 |
| Tests: `test json-properties` | 93 | 131 | 219 | 145 | 86 |
| Tests: `rs rung-disable` | 124 | 369 | 110 | 105 | 117 |
| Tests: `rs multicast` | 366 | 178 | 210 | 192 | 158 |
| Tests: `rs v4l2` | 80 | 0 | 146 | 125 | 76 |
| Tests: `rs render-sw` | 220 | 115 | 143 | 164 | 90 |
| Tests: `rs send-depth` | 0 | 0 | 0 | 172 | 4 |
| Tests: `test drain` (new) | | | | 142 | 90 |
| Tests: `test route-sweep` (new) | | | | 96 | 17 |
| Nightly: Binding benchmark runners | 212 | 227 | 193 | 481 | 162 |
| Nightly: Rust bench targets (cancelled) | 761 | 789 | 894 | 837 | 743 |
| Nightly: Rust shared bench targets | 874 | 937 | 800 | 951 | 885 |
| Nightly: Relay session benchmarks (cancelled) | 801 | 861 | 882 | 939 | 871 |
| Nightly: Rust supplemental benchmarks | 983 | 1,369 | 1,296 | 1,282 | 1,147 |
| Nightly: io_uring benchmarks | 737 | 927 | 1,017 | 909 | 873 |
| Nightly: Browser benchmark smoke | 146 | 66 | 62 | 158 | 40 |
| Nightly: Go audit | 159 | 161 | 0 | 124 | 74 |
| Nightly: OBS | 563 | 555 | 585 | 567 | 544 |
| Nightly: OBS TSan | 170 | 109 | 178 | 151 | 79 |
| Nightly: moq-c C fixtures | 122 | 117 | 288 | 269 | 49 |
| Nightly: Rust benchmark tests (split since) | 400 | 397 | 341 | 156 | 141 |
| Nightly: Rust application benchmark tests (new) | | | | 150 | 104 |
| Media (firefox player) | 58 | 84 | | 180 | 35 |
| Audio quality (three builds summed) | 151 | 154 | 79 | 303 | 130 |
| Shipping sizes (skipped until 11-13) | | | | | 104 |

The sums above leave out the rows marked new or split and `Shipping sizes`,
which have no comparable baseline.

What the after-#8631 nights must show here, read from the same summaries
the [failed-puts quest](/quest/m0/ci/mbx-failed-puts.md) already collects for
its own after-measurement:

- Windows: whether its workspace build hits most looked-up keys once its
  download stays under 20 GiB with no budget refusals. The first such
  night settles the upstream report (Waiting).
- Linux: whether full history now raises each job's hits above its depth-1
  range. If the jobs still sit inside it once restores stop failing, plan a
  quest that returns them to `fetch-depth: 1`, since full history then costs
  checkout time for nothing.
- Pick nights at least a week after 11-13, or note where the expiry wave
  still shows.
- The Goal's same-commit restore-plus-build comparison is still unmeasured;
  take it from two scheduled runs on one `main` commit if a quiet day gives
  them, else say none occurred.

Draft upstream issue for jdx/mr-boxington, the fallback the Waiting item
files only if Windows still misses most keys without budget refusals. Its CONTRIBUTING.md accepts bug reports in Issues or
Discussions only from real use, and requires AI disclosure. Its AGENTS.md
gives the disclosure line, which the poster appends verbatim with its own
runtime identifiers, followed by the `(Written by <model> with <effort>)`
line. Cite only logs and behavior; no source code. Replace the bracketed
placeholder with the first night after #8631.

> **Title:** Windows CI: most predicted compilations miss in a remote that the previous night's same job wrote
>
> **What I was trying to do:** Share one S3-compatible remote (Cloudflare R2) between nightly GitHub-hosted jobs, namespaced per runner OS and arch, through mr-boxington-action with mbx 1.30.0, `MBX_REMOTE_MODE=read-write` and `MBX_GC_MAX_SIZE=20GiB`. The Windows job (`windows-latest`, x86_64-pc-windows-msvc) runs `cargo check --locked --workspace --all-targets`, and the same job wrote the remote the night before with at most a few `Cargo.lock` changes in between.
>
> **What happened:** macOS restores most of what it looks up; Windows restores about one in ten:
>
> ```
> macOS   mbx[cache]: object cache: 989 hits, 141 misses, 59 not looked up, ...
> Windows mbx[cache]: object cache: 116 hits, 1030 misses, 19 not looked up, 257 prefetched, 3 bypassed, 1 remote failures; 10.0 GiB downloaded, 1.2 GiB uploaded, 11.2 GiB stored locally
> Windows, after raising the download budget: [summary line from the first night after moq-dev/moq#8631]
> ```
>
> `mbx analyze` after the build reports all of them as `misses with no earlier recording`, which is expected on a fresh runner, so it doesn't say whether the remote held those keys. One earlier Windows night on 1.29.2, under the default 10 GiB budget, had 846 hits and 224 misses at 8.7 GiB downloaded.
>
> **What would help:** a way to tell, for a predicted compilation that missed, whether the remote held an action under that key, and which task manifest the build inherited its predictions from.

## Waiting

- GitHub Actions: the first scheduled Nightly after #8631 (expected
  2026-11-14) settles the Windows upstream report, by its Windows workspace
  build's `mbx[cache]: object cache` line and any `download budget exceeded`
  warnings. Record that night's numbers in the Plan, then:
  - If Windows hits most looked-up keys, or still misses with budget
    refusals, comment on
    [jdx/mr-boxington#950](https://github.com/jdx/mr-boxington/discussions/950)
    only if the numbers add something it lacks, such as Windows hits
    recovering once the budget stops binding, or refusals at 20 GiB. Skip it
    otherwise, and file no issue.
  - If Windows still misses most looked-up keys with no budget refusals, file
    the drafted issue (Plan), its placeholder filled from that night.

  The maintainer approved both posts. Each follows mr-boxington's
  CONTRIBUTING.md and AGENTS.md AI-disclosure rules and ends with the
  `(Written by <model> with <effort>)` line. Link what was posted, or say
  nothing was, in the Plan, then drop this item.
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
