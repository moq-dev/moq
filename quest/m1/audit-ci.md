# [M] CI workflow audit

## Goal

Every CI workflow and job earns its place: its results reach someone (a gate,
a Discord alert, or a published report) at a cost proportional to that signal.
Each one that doesn't is fixed, moved, or deleted. Individual tests stay with
the [test cost audit](/quest/m2/audit-test-cost.md), and runner and cache
infrastructure with [CI cost and latency](/quest/m0/ci/README.md).

## Plan

Recurs once per major release (decided 2026-10-20). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2), never m3, the deferred bucket
that does not shift with releases. The successor keeps the Goal and this
paragraph; the rest of its Plan is rewritten with what this run learned: the
per-workflow table, baselines, and what to check first. A successor placed in m2 lists the
[release cut](/quest/m2/release/cut.md) under `Required`, so it waits for the
release in flight to ship. The parent milestone lists this quest under
`Required`. Only `/quest-delete` ends the recurrence. Fixes land in the run's
PR, one commit each, rather than as follow-ups. Only a change that needs a
maintainer decision (dropping a release target, scope) or is independently
landable and too large to review alongside the rest becomes a follow-up
through `/quest-plan`.

Split from the test cost audit on 2026-10-20: this audit owns workflows and
jobs, that one owns tests. It also owns job-level build cost, which the
one-time m0 CI pipeline audit handed over on 2026-10-21 after measuring the
one-pass queue.

Each run tables every workflow in `.github/workflows/` and every Nightly job
with three verdicts:

- **Signal:** what regression it catches that nothing cheaper catches, and
  when it last caught one.
- **Visibility:** who sees a failure or a result. A gate blocks merging, a
  scheduled or publishing workflow posts to Discord `#alerts` through
  [alert.yml](/.github/workflows/alert.yml) (`just gh check` enforces its
  coverage), and a report lands on the
  [Pages site](https://git.moq.dev/) or in a job summary. A result
  only in a run artifact or log is invisible: publish it, summarize it, or
  stop producing it. Each report on the Pages site owns one `gh-pages`
  directory: its workflow commits only that directory, deploys the whole
  branch, and joins the `benchmark-pages` concurrency group with
  `queue: max`, so no deploy drops another report (decided 2026-10-20; the
  benchmark, relay, and interop reports do, through `sh/gh/pages-publish.sh`).
- **Cost:** runner-minutes per month by runner type, from the org billing API
  and job timings, against that signal. macOS is the scarce one (a 5-job
  concurrency cap).

Check first (from the test cost audit's m0 run, 2026-10-19):

- Classify every failed job of the latest Nightly, scheduled Interop, and
  Platform runs, not just the slowest. On 2026-10-19, 28 Nightly jobs had
  failed for eight independent causes, some for days. A red Nightly hides the
  next regression: the send-depth gate and all six fuzz targets broke on
  2026-10-18 and stayed broken unnoticed until that audit. Nightly was still
  red on 2026-10-20 (fuzz, media players, audit, features, release builds), so
  an alert that always fires is not visibility either.
- Platform (macOS, Windows) runs only in the merge queue and is not required,
  so a Windows-only compile error in moq-video stayed red for two days across
  dozens of queue runs until
  [#5966](https://github.com/moq-dev/moq/pull/5966). Most Platform failures
  in the sample were that error, a choco `nasm` install outage, or another
  queued PR's breakage. Rank Platform by failure cause, not count.
- A queue failure often belongs to another PR in the same batch: a stale
  fixture in the entry itself, or a semantic conflict with a PR merged after it
  was queued. Check the entry's head branch and what merged around it before
  calling it a flake.
- Owned elsewhere: Rust advisories by
  [the dependency audit](/quest/m2/audit-deps.md). The release toolchain now
  links.
- Re-running a single job in place cannot redeploy Pages: the earlier
  attempt's `github-pages` artifact remains and `deploy-pages` refuses two.
  Delete the stale artifact first, or wait for the next Nightly.

Check first (from the m0 CI pipeline audit, 2026-10-21):

- The queue runs one `just ci` job on the persistent `nix/runner` VM. Over 32
  entries it took a median 5.5 minutes, p90 11.9, at most 24.2. Its job
  summary lists seconds per module, and the `cargo-timings` artifact is the
  per-crate drill-down behind that table. Re-measure from those, not from log
  timestamps.
- An `rs/moq-ffi` change selects every binding, and the bindings were about
  half of the slowest entries: C++ with the OBS plugin about 4.5 minutes,
  Dart 45 to 160 seconds, Kotlin 30 to 110, Go about a minute.
  [moq-dev/moq#6247](https://github.com/moq-dev/moq/pull/6247) compiles
  `moq.cpp` twice instead of six times and keeps the Gradle daemon warm
  across queue jobs; re-measure both. The dispatcher runs modules one after
  another, and most binding steps use one core of eight.
- A core Rust change (moq-net, moq-quic, moq-tokio) spends 3 to 4.5 minutes
  compiling the test build and 2 to 2.5 running it, plus separate builds for
  `cargo doc` (25 to 60 s), the io_uring clippy (20 to 50 s), the wasm32
  clippy (12 s), and the capture (30 to 36 s) and play (40 to 48 s) tests.
- Deferred to the maintainer (coverage trade-off): fold the capture and play
  test builds into the main test build, as the platform build already does
  (about 75 s per core Rust entry; the non-feature arms of moq-video,
  moq-audio and moq-cli would compile only in Nightly's Features job, and
  openh264 in `dev` tests needs measuring first), or move both to Nightly
  (the same saving, but the gate stops testing the device features). The
  m0 audit recommended folding; the queue keeps both builds until decided.
- Cold builds (release PRs, Nightly) still compile both rustls backends and
  aws-lc-sys once per environment; the queue VM keeps those built.
- Nightly on 2026-10-20 took about 1,450 runner-minutes (320 macOS) and was
  red for ten causes. Features' clippy was fixed by
  [#6117](https://github.com/moq-dev/moq/pull/6117) (its 2026-10-21 failure
  is in tests). `gh test` failed again on 2026-10-21 after
  [#6086](https://github.com/moq-dev/moq/pull/6086), then
  [#6154](https://github.com/moq-dev/moq/pull/6154) removed the assertion.
  `rs audio-devices` failed all five playback tests on the hosted ARM runner
  on 2026-10-20 and passed on the 2026-10-21 Nightly; it is owned by
  [Audio devices on ARM](/quest/m1/audio-devices-arm.md). Owned elsewhere
  besides the above: the Rust and JS benchmark legs by the
  [performance audit](/quest/m1/audit-perf.md), Audio quality's budgets by
  the [audio jitter target](/quest/m1/audio-jitter-target/README.md), and the
  bun advisories with the Rust ones.

### Audit checkpoint: 2026-10-21

This run is unfinished. Investigation baseline was main `766934221b` after the
dependency audit; the checkpoint rebased cleanly onto main `bce217e063` afterward.
API data was retrieved on 2026-10-22 UTC (2026-10-21 in America/Los_Angeles).

Six independent fixes are checkpointed: setup-gate assertions accept the correctly
gated timing artifact after Check; feature assertions count the actual three boundary
and twelve exhaustive Tokio lints; the generator comparison now appends its existing
Markdown table to the Actions job summary instead of leaving its numbers only in logs
and an artifact; the archive producer benchmark is now included in the existing
Nightly Criterion group after the tooling suite exposed its omission. All three
failing assertions were reproduced before their source or fixture fixes. Rust and JS
coverage, browser baseline medians and browser benchmark comparisons now also reach
Actions job summaries; their schedules, raw artifacts and correctness rules stay intact.

#### Cost baseline and limits

Org billing, filtered to this repository, reports October usage through the partial
2026-10-22 UTC day: Linux 116,552 minutes, Linux ARM 221,937, macOS 54,829,
Windows 52,651. All net charges are zero after discounts. These are runner capacity
costs, not an estimate of a payment. No other repository usage is included here.

The per-workflow timing endpoint returns an empty billable object and the run timing
endpoint returns zero milliseconds for these public runs. Per-workflow monthly minute
attribution therefore remains unmeasured. The tables use measured job start/end times;
October invocation counts can include skipped runs and old workflow definitions and
are capped by GitHub at 2,500. Multiplying them by one sample would be misleading.

The latest completed scheduled [Nightly](https://github.com/moq-dev/moq/actions/runs/39600361153)
is attempt 1 at `bc64b9c52561`: 166 jobs, 122 passed, 18 failed, 26 skipped.
It consumed 1,372.2 elapsed runner-minutes: ARM 636.1, Linux 259.8, macOS 289.8,
Windows 186.4. At 31 identical nights that would be about 42,538 runner-minutes,
including 8,985 macOS minutes. This is a schedule projection, not measured monthly
attribution or a forecast of changing builds. A macOS five-job cap also makes queue
delay a separate concern from executed minutes.

The latest scheduled [Interop](https://github.com/moq-dev/moq/actions/runs/39608646193)
and latest standalone [Platform](https://github.com/moq-dev/moq/actions/runs/39672105125)
passed every executed job. The latter ran both macOS and Windows. The successful
ARM audio-device job in the sampled Nightly predates
[the PulseAudio period fix](https://github.com/moq-dev/moq/pull/6175); it does not
validate that fix on ARM. Audio budget validation likewise still needs new nights.

#### Every current workflow

All 49 checked-in workflow definitions are listed. Callable workflows are also
costed in their parent runs; their historical standalone samples are not extra current
Nightly cost. A success is coverage evidence, not proof of a regression caught.
The last recorded failure is linked for each workflow with retained history. These
can be old PR failures; they are not automatically unique regressions in the current
workflow. No retained failure means no demonstrated recent catch, not zero value.
Alert coverage passes `just gh check`; a permanent red job still needs cause triage.

| Workflow | Signal | Visibility | October invocations | Latest direct job minutes by runner | Last recorded failure |
| --- | --- | --- | ---: | --- | --- |
| [alert](/.github/workflows/alert.yml) | Scheduled/publishing failure delivery; delivery and coverage guards | Discord failure notice; coverage checked locally | at least 2,500 | no executed job sample | [2026-10-05](https://github.com/moq-dev/moq/actions/runs/37369105130) |
| [android](/.github/workflows/android.yml) | Android cross-compilation and packaging beyond the native gate | Caller gate and Nightly alert | at least 2,500 | Linux 1.9 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39343452442) |
| [apt-repo](/.github/workflows/apt-repo.yml) | APT repository metadata, signatures and publication | Published repository/installer and alert | 23 | Linux 1.5 | [2026-05-24](https://github.com/moq-dev/moq/actions/runs/26349324650) |
| [audio-quality](/.github/workflows/audio-quality.yml) | Deterministic audio replay correctness; real-time quality is in Nightly | Caller gate and Nightly alert | 1342 | ARM 16.4 | [2026-10-14](https://github.com/moq-dev/moq/actions/runs/38530745049) |
| [check](/.github/workflows/check.yml) | Required diff-scoped merge admission; module timing summary | Required Check and module timing summary | at least 2,500 | no executed job sample | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39721515505) |
| [cpp](/.github/workflows/cpp.yml) | C++ compiler and ABI consumer builds; not only Rust compilation | Caller gate and Nightly alert | 811 | no executed job sample | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39343452453) |
| [dependabot](/.github/workflows/dependabot.yml) | Dependency PR policy and maintenance automation | Dependency PR state | at least 2,500 | no executed job sample | No retained standalone failure |
| [docker](/.github/workflows/docker.yml) | Container image build and multi-architecture publication | Caller gate/alert and release artifacts | 16 | ARM 22.6, Linux 29.7 | [2026-10-05](https://github.com/moq-dev/moq/actions/runs/37382213733) |
| [features](/.github/workflows/features.yml) | Feature isolation and missing dependency gates | Caller gate and Nightly alert | 0 | no executed job sample | No retained standalone failure |
| [interop](/.github/workflows/interop.yml) | Cross-language protocol/media behavior; latest scheduled run passed | Discord and Pages interop report | 743 | Linux 21.6 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39276932602) |
| [moq-c](/.github/workflows/moq-c.yml) | C ABI packaging, fixtures and consumer links | Caller gate/alert and release artifacts | 0 | no executed job sample | No retained standalone failure |
| [moq-cli](/.github/workflows/moq-cli.yml) | CLI release archives and target-specific links | Caller gate/alert and release artifacts | 8 | ARM 6.5, Linux 6.2, Windows 17.1, macOS 2.6 | [2026-10-16](https://github.com/moq-dev/moq/actions/runs/38854842213) |
| [moq-gst](/.github/workflows/moq-gst.yml) | GStreamer plugin packaging and dynamic links | Caller gate/alert and release artifacts | 11 | ARM 9.4, Linux 10.3, macOS 10.8 | [2026-10-17](https://github.com/moq-dev/moq/actions/runs/39000627149) |
| [moq-relay](/.github/workflows/moq-relay.yml) | Relay release archives and target-specific links | Caller gate/alert and release artifacts | 8 | ARM 5.1, Linux 5.3, Windows 9.3, macOS 44.1 | [2026-09-30](https://github.com/moq-dev/moq/actions/runs/36747636505) |
| [moq-sh](/.github/workflows/moq-sh.yml) | Installer publication, fixtures and installed artifact smoke | Published repository/installer and alert | 13 | Linux 0.2 | No retained standalone failure |
| [nightly](/.github/workflows/nightly.yml) | Diff-independent regression suite and release dry-runs; classified below | Discord, Pages benchmarks, summary; gaps below | 26 | ARM 600.2, Linux 168.0, Windows 98.2, macOS 206.5 | [2026-10-21](https://github.com/moq-dev/moq/actions/runs/39600361153) |
| [obs](/.github/workflows/obs.yml) | OBS consumer build and plugin packaging | Caller gate and Nightly alert | 2103 | ARM 22.2, Windows 12.9, macOS 10.9 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39343452410) |
| [oss-scanner](/.github/workflows/oss-scanner.yml) | OSS Scanner container builds | Caller gate and Nightly alert | 1 | ARM 9.7 | No retained standalone failure |
| [platform](/.github/workflows/platform.yml) | macOS/Windows compile, bindings and native tests | Caller gate and Nightly alert | at least 2,500 | Windows 12.3, macOS 7.0 | [2026-10-20](https://github.com/moq-dev/moq/actions/runs/39353885165) |
| [quest](/.github/workflows/quest.yml) | Unscoped quest-tree validation after direct pushes | Quest check and push-failure alert | at least 2,500 | ARM 0.5 | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39727308653) |
| [release-binary](/.github/workflows/release-binary.yml) | Shared target-specific binary packaging | Calling release workflow; not independently triggered | 0 | no executed job sample | No retained standalone failure |
| [release-brew](/.github/workflows/release-brew.yml) | Homebrew publication | Caller gate/alert and release artifacts | 27 | Linux 0.2 | No retained standalone failure |
| [release-cpp](/.github/workflows/release-cpp.yml) | C++ release artifacts and consumers | Caller gate/alert and release artifacts | 2 | ARM 4.7, Linux 26.4, Windows 32.7, macOS 21.4 | No retained standalone failure |
| [release-dart-ffi](/.github/workflows/release-dart-ffi.yml) | Dart native FFI artifacts | Caller gate/alert and release artifacts | 293 | Linux 45.0 | [2026-10-10](https://github.com/moq-dev/moq/actions/runs/38088792983) |
| [release-dart](/.github/workflows/release-dart.yml) | Dart package publication | Caller gate/alert and release artifacts | 198 | Linux 0.3 | [2026-10-16](https://github.com/moq-dev/moq/actions/runs/38973399435) |
| [release-ffi](/.github/workflows/release-ffi.yml) | Shared FFI target builder | Calling release workflow; not independently triggered | 0 | no executed job sample | No retained standalone failure |
| [release-gate](/.github/workflows/release-gate.yml) | Complete release PR admission, including callable platform lanes | Caller gate/alert and release artifacts | 0 | no executed job sample | No retained standalone failure |
| [release-go-ffi](/.github/workflows/release-go-ffi.yml) | Go native FFI artifacts | Caller gate/alert and release artifacts | 58 | Linux 33.5, Windows 17.4, macOS 14.0 | [2026-10-18](https://github.com/moq-dev/moq/actions/runs/39135382582) |
| [release-go](/.github/workflows/release-go.yml) | Go wrapper publication | Caller gate/alert and release artifacts | 179 | Linux 1.3 | [2026-10-14](https://github.com/moq-dev/moq/actions/runs/38638664735) |
| [release-js](/.github/workflows/release-js.yml) | npm/JSR packages and exports | Caller gate/alert and release artifacts | 1391 | Linux 16.9 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39304286923) |
| [release-kt-ffi](/.github/workflows/release-kt-ffi.yml) | Kotlin native FFI artifacts | Caller gate/alert and release artifacts | 8 | Linux 40.4, Windows 16.8, macOS 13.4 | [2026-09-14](https://github.com/moq-dev/moq/actions/runs/34897543024) |
| [release-kt-lib](/.github/workflows/release-kt-lib.yml) | Kotlin wrapper artifacts | Caller gate/alert and release artifacts | 281 | Linux 14.8 | [2026-10-16](https://github.com/moq-dev/moq/actions/runs/38828705077) |
| [release-nix](/.github/workflows/release-nix.yml) | Nix release evaluation/publication | Caller gate/alert and release artifacts | 0 | no executed job sample | No retained standalone failure |
| [release-py-ffi](/.github/workflows/release-py-ffi.yml) | Python native wheels | Caller gate/alert and release artifacts | 8 | ARM 7.0, Linux 8.0, Windows 14.4, macOS 6.9 | [2026-09-14](https://github.com/moq-dev/moq/actions/runs/34881733830) |
| [release-py](/.github/workflows/release-py.yml) | Python wrapper package | Caller gate/alert and release artifacts | 8 | Linux 17.0 | [2026-05-20](https://github.com/moq-dev/moq/actions/runs/26167963152) |
| [release-rs](/.github/workflows/release-rs.yml) | Rust package release policy | Caller gate/alert and release artifacts | 86 | Linux 21.9 | [2026-10-06](https://github.com/moq-dev/moq/actions/runs/37396594590) |
| [release-shared-ffi](/.github/workflows/release-shared-ffi.yml) | Shared native FFI archives | Caller gate/alert and release artifacts | 0 | no executed job sample | No retained standalone failure |
| [release-swift-ffi](/.github/workflows/release-swift-ffi.yml) | Swift native FFI artifacts | Caller gate/alert and release artifacts | 8 | Linux 6.4, macOS 27.6 | [2026-08-08](https://github.com/moq-dev/moq/actions/runs/31232064868) |
| [release-swift-lib](/.github/workflows/release-swift-lib.yml) | Swift wrapper publication | Caller gate/alert and release artifacts | 97 | Linux 0.1, macOS 0.8 | [2026-09-22](https://github.com/moq-dev/moq/actions/runs/35742256991) |
| [release-winget](/.github/workflows/release-winget.yml) | Winget manifests and publication | Caller gate/alert and release artifacts | 16 | Windows 0.3 | [2026-10-17](https://github.com/moq-dev/moq/actions/runs/39066762564) |
| [rpm-repo](/.github/workflows/rpm-repo.yml) | RPM repository metadata/signatures/publication | Published repository/installer and alert | 23 | Linux 2.3 | [2026-05-25](https://github.com/moq-dev/moq/actions/runs/26377814471) |
| [runner-vm](/.github/workflows/runner-vm.yml) | Persistent runner VM configuration and boot/image checks | Caller gate and Nightly alert | 4 | Linux 3.2 | No retained standalone failure |
| [smoke-toolchain](/.github/workflows/smoke-toolchain.yml) | Coupled smoke tooling and Playwright update validation | Update PR and scheduled failure alert | 0 | no executed job sample | No retained standalone failure |
| [smoke](/.github/workflows/smoke.yml) | Published distribution and external-relay behavior | Discord and Pages relay report | 21 | Linux 95.6 | [2026-09-25](https://github.com/moq-dev/moq/actions/runs/36076458401) |
| [ts](/.github/workflows/ts.yml) | TS container compliance and round-trip media integrity | Scheduled failure alert and release gate | 1493 | Linux 34.9 | [2026-10-21](https://github.com/moq-dev/moq/actions/runs/39607267367) |
| [uring](/.github/workflows/uring.yml) | io_uring compile and runtime/kernel contract | Caller gate and Nightly alert | 1711 | ARM 7.7 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39344299025) |
| [video](/.github/workflows/video.yml) | Browser video renderer behavior | Scheduled failure alert and release gate | 413 | Linux 1.9 | No retained standalone failure |
| [wasm](/.github/workflows/wasm.yml) | Native/wasm cross-language bindings and browser behavior | Caller gate and Nightly alert | at least 2,500 | ARM 23.8 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39343452404) |
| [x11](/.github/workflows/x11.yml) | SHM and GetImage native capture paths | Caller gate and Nightly alert | 680 | ARM 5.2 | [2026-10-17](https://github.com/moq-dev/moq/actions/runs/39002028489) |

#### Every job in the sampled Nightly

Cost is elapsed minutes per invocation, excluding queue delay, not billed minutes.
The job name identifies the target or recipe that supplies the signal. All failures
reach Nightly/Discord; the last catch is the sampled failure for the red rows.
For green rows the last regression caught is not established by this snapshot.
Skipped publication jobs are expected in dry-runs and consumed no measured minutes.
The current workflow has changed since this sample, including removal of WebKit.

| Job | Signal | Visibility | Result | Runner minutes |
| --- | --- | --- | --- | ---: |
| [Media (webkit player)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223009919) | Browser media and negative controls | Caller verdict/Discord | failure | 5.8 (ubuntu-24.04-arm) |
| [Fuzz (ietf)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010246) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 9.1 (ubuntu-24.04-arm) |
| [Release Kotlin lib / Check FFI publisher](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010305) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.2 (ubuntu-latest) |
| [Fuzz (path)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010330) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 9.2 (ubuntu-24.04-arm) |
| [Fuzz (connection)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010336) | Fresh malformed-input/crash detection | Caller verdict/Discord | failure | 7.7 (ubuntu-24.04-arm) |
| [Fuzz (streams)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010351) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 7.9 (ubuntu-24.04-arm) |
| [Media (firefox player)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010354) | Browser media and negative controls | Caller verdict/Discord | failure | 8.7 (ubuntu-24.04-arm) |
| [Fuzz (lite)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010359) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 9.4 (ubuntu-24.04-arm) |
| [Tests (rs render-sw)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010363) | Named diff-independent suite | Caller verdict/Discord | success | 3.2 (ubuntu-24.04-arm) |
| [Docker / Parse tag](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010385) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [Fuzz (streamid)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010409) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 7.9 (ubuntu-24.04-arm) |
| [Nightly (BBB rendition alignment)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010416) | Workload correctness/benchmark validity | Caller verdict/Discord | success | 1.4 (ubuntu-24.04-arm) |
| [Nightly (Relay shard sweep)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010417) | Workload correctness/benchmark validity | Caller verdict/Discord | success | 14.4 (ubuntu-24.04-arm) |
| [Tests (rs doctest --workspace)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010424) | Named diff-independent suite | Caller verdict/Discord | success | 6.6 (ubuntu-24.04-arm) |
| [Fuzz (pattern)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010427) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 9.0 (ubuntu-24.04-arm) |
| [Nightly (JS benchmarks)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010433) | Workload correctness/benchmark validity | Pages benchmark history and status | failure | 3.3 (ubuntu-24.04-arm) |
| [Nightly (Binding benchmark)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010434) | Workload correctness/benchmark validity | Pages benchmark history and status | success | 44.0 (ubuntu-24.04-arm) |
| [Nightly (Audio grouping relay benchmark)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010437) | Workload correctness/benchmark validity | Caller verdict/Discord | success | 17.4 (ubuntu-24.04-arm) |
| [Audio quality](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010452) | Real-time listener quality | Enforced browser verdict; native rows need summary | success | 47.9 (ubuntu-24.04-arm) |
| [Fuzz (frame)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010454) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 7.8 (ubuntu-24.04-arm) |
| [Nightly (JS audit)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010455) | Time-sensitive advisories | Failure/Discord; dependency audit owns findings | failure | 1.1 (ubuntu-24.04-arm) |
| [Fuzz (announce)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010487) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 9.2 (ubuntu-24.04-arm) |
| [Tests (rs quic-sim)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010505) | Named diff-independent suite | Caller verdict/Discord | success | 4.2 (ubuntu-24.04-arm) |
| [Nightly (JS coverage)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010513) | Workload correctness/benchmark validity | Job summary after this PR; raw traces in artifact | success | 2.4 (ubuntu-24.04-arm) |
| [Nightly (Generator comparison)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010518) | Workload correctness/benchmark validity | Job summary after this PR | success | 24.9 (ubuntu-24.04-arm) |
| [Nightly (Audit)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010523) | Time-sensitive advisories | Failure/Discord; dependency audit owns findings | failure | 1.4 (ubuntu-24.04-arm) |
| [moq-c / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010532) | Target build, packaging or release contract | Caller verdict/Discord | success | 11.6 (ubuntu-24.04-arm) |
| [moq-c / Build (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010536) | Target build, packaging or release contract | Caller verdict/Discord | success | 25.5 (macos-latest) |
| [Nightly (Browser backend baseline)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010537) | Workload correctness/benchmark validity | Job summary after this PR; raw traces in artifact | success | 17.0 (ubuntu-24.04-arm) |
| [Nightly (io_uring benchmarks)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010543) | Workload correctness/benchmark validity | Pages benchmark history and status | success | 7.9 (ubuntu-24.04-arm) |
| [Nightly (Rust supplemental benchmarks)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010563) | Workload correctness/benchmark validity | Pages benchmark history and status | success | 15.7 (ubuntu-24.04-arm) |
| [moq-c / Build (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010595) | Target build, packaging or release contract | Caller verdict/Discord | success | 17.8 (windows-latest) |
| [Tests (gate)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010596) | Named diff-independent suite | Caller verdict/Discord | failure | 8.6 (ubuntu-24.04-arm) |
| [Nightly (OBS)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010646) | Workload correctness/benchmark validity | Caller verdict/Discord | success | 7.9 (ubuntu-24.04-arm) |
| [Tests (gh test)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010675) | Named diff-independent suite | Caller verdict/Discord | failure | 1.4 (ubuntu-24.04-arm) |
| [Tests (test json-properties)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010677) | Named diff-independent suite | Caller verdict/Discord | success | 2.7 (ubuntu-24.04-arm) |
| [Tests (rs multicast)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010683) | Named diff-independent suite | Caller verdict/Discord | success | 6.0 (ubuntu-24.04-arm) |
| [Nightly (moq-c C fixtures)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010690) | Workload correctness/benchmark validity | Caller verdict/Discord | success | 3.8 (ubuntu-24.04-arm) |
| [moq-c / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010694) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.3 (ubuntu-latest) |
| [Nightly (Rust bench targets)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010720) | Workload correctness/benchmark validity | Pages benchmark history and status | success | 36.4 (ubuntu-24.04-arm) |
| [Tests (rs audio-devices)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010747) | Named diff-independent suite | Caller verdict/Discord | success | 2.8 (ubuntu-24.04-arm) |
| [Fuzz (params)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010750) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 8.1 (ubuntu-24.04-arm) |
| [moq-gst / Build tarball (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010753) | Target build, packaging or release contract | Caller verdict/Discord | success | 9.2 (ubuntu-latest) |
| [Fuzz (packet)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010755) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 8.1 (ubuntu-24.04-arm) |
| [Tests (rs send-depth)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010811) | Named diff-independent suite | Caller verdict/Discord | success | 1.9 (ubuntu-24.04-arm) |
| [Tests (test drain)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010816) | Named diff-independent suite | Caller verdict/Discord | success | 3.2 (ubuntu-24.04-arm) |
| [moq-gst / Package .deb/.rpm (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010831) | Target build, packaging or release contract | Caller verdict/Discord | failure | 5.5 (ubuntu-24.04-arm) |
| [Nightly (Rust benchmark tests)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010855) | Workload correctness/benchmark validity | Caller verdict/Discord | failure | 27.9 (ubuntu-24.04-arm) |
| [moq-gst / Package .deb/.rpm (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010878) | Target build, packaging or release contract | Caller verdict/Discord | failure | 6.3 (ubuntu-24.04) |
| [Tests (rs uring)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010879) | Named diff-independent suite | Caller verdict/Discord | success | 8.1 (ubuntu-24.04-arm) |
| [Tests (rs loom)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010930) | Named diff-independent suite | Caller verdict/Discord | failure | 3.9 (ubuntu-24.04-arm) |
| [Nightly (Browser benchmark smoke)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010964) | Workload correctness/benchmark validity | Job summary after this PR; raw traces in artifact | failure | 11.4 (ubuntu-24.04-arm) |
| [Nightly (Rust coverage)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010968) | Workload correctness/benchmark validity | Job summary after this PR; raw traces in artifact | success | 20.4 (ubuntu-24.04-arm) |
| [Nightly (Relay profiles)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010993) | Workload correctness/benchmark validity | Job summary after this PR; raw traces in artifact | success | 23.4 (ubuntu-24.04) |
| [moq-gst / Build tarball (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011023) | Target build, packaging or release contract | Caller verdict/Discord | success | 7.5 (ubuntu-24.04-arm) |
| [moq-gst / Build tarball (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011100) | Target build, packaging or release contract | Caller verdict/Discord | success | 23.3 (macos-latest) |
| [Tests (test drill-sensitivity)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011123) | Named diff-independent suite | Caller verdict/Discord | success | 14.3 (ubuntu-24.04-arm) |
| [runner-vm / check](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011144) | Target build, packaging or release contract | Caller verdict/Discord | success | 3.8 (ubuntu-24.04) |
| [audio-replay / Replay](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011466) | Deterministic audio target replay | Caller verdict/Discord | success | 1.2 (ubuntu-24.04-arm) |
| [android / Android](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011489) | Target build, packaging or release contract | Caller verdict/Discord | success | 3.1 (ubuntu-24.04) |
| [Tests (rs quic-stress)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011515) | Named diff-independent suite | Caller verdict/Discord | success | 3.6 (ubuntu-24.04-arm) |
| [wasm / WASM](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011524) | Target build, packaging or release contract | Caller verdict/Discord | success | 5.6 (ubuntu-24.04-arm) |
| [x11 / SHM and GetImage](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011656) | Target build, packaging or release contract | Caller verdict/Discord | success | 5.5 (ubuntu-24.04-arm) |
| [features / Features (js wasm)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011688) | Feature and profile isolation | Caller verdict/Discord | success | 2.5 (ubuntu-24.04-arm) |
| [Fuzz (varint)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011710) | Fresh malformed-input/crash detection | Caller verdict/Discord | success | 9.1 (ubuntu-24.04-arm) |
| [features / Features (rs features)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011824) | Feature and profile isolation | Caller verdict/Discord | failure | 35.0 (ubuntu-24.04-arm) |
| [Release JS / Release JS Packages](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011874) | Target build, packaging or release contract | Caller verdict/Discord | success | 1.7 (ubuntu-latest) |
| [platform / Platform scope](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011877) | macOS/Windows native coverage | Caller verdict/Discord | success | 0.1 (ubuntu-24.04-arm) |
| [cpp / C++ (clang)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223011997) | Target build, packaging or release contract | Caller verdict/Discord | success | 6.4 (ubuntu-24.04) |
| [Release Dart / Check package](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012142) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.4 (ubuntu-latest) |
| [oss-scanner / Build](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012255) | Target build, packaging or release contract | Caller verdict/Discord | success | 9.9 (ubuntu-24.04-arm) |
| [Release Py FFI / Parse version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012301) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [Release Swift Lib / Check FFI publisher](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012303) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [moq-cli / release / Build Windows zip (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012739) | Target build, packaging or release contract | Caller verdict/Discord | success | 26.4 (windows-latest) |
| [moq-cli / release / Build macOS tarball (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012757) | Target build, packaging or release contract | Caller verdict/Discord | success | 37.1 (macos-latest) |
| [Release FFI / Shared moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012788) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [moq-cli / release / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012876) | Target build, packaging or release contract | Caller verdict/Discord | failure | 14.2 (ubuntu-22.04) |
| [moq-relay / release / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012937) | Target build, packaging or release contract | Caller verdict/Discord | failure | 10.1 (ubuntu-22.04) |
| [moq-cli / release / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012941) | Target build, packaging or release contract | Caller verdict/Discord | failure | 12.9 (ubuntu-22.04-arm) |
| [moq-relay / release / Build macOS tarball (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012992) | Target build, packaging or release contract | Caller verdict/Discord | success | 35.0 (macos-latest) |
| [moq-relay / release / Build Windows zip (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012993) | Target build, packaging or release contract | Caller verdict/Discord | success | 14.3 (windows-latest) |
| [Release Dart FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223012995) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [moq-relay / release / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223013035) | Target build, packaging or release contract | Caller verdict/Discord | failure | 9.1 (ubuntu-22.04-arm) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095136) | Target build, packaging or release contract | Caller verdict/Discord | success | 15.1 (macos-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-linux-android)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095143) | Target build, packaging or release contract | Caller verdict/Discord | success | 8.2 (ubuntu-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095148) | Target build, packaging or release contract | Caller verdict/Discord | success | 11.7 (ubuntu-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-apple-ios-sim)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095191) | Target build, packaging or release contract | Caller verdict/Discord | success | 13.3 (macos-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-apple-ios)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095192) | Target build, packaging or release contract | Caller verdict/Discord | success | 11.1 (macos-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-apple-ios)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095213) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.8 (macos-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-linux-android)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095263) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.4 (ubuntu-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095283) | Target build, packaging or release contract | Caller verdict/Discord | success | 15.9 (macos-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (armv7-linux-androideabi)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095295) | Target build, packaging or release contract | Caller verdict/Discord | success | 7.3 (ubuntu-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095331) | Target build, packaging or release contract | Caller verdict/Discord | success | 21.1 (windows-latest) |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223095405) | Target build, packaging or release contract | Caller verdict/Discord | success | 8.6 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097568) | Target build, packaging or release contract | Caller verdict/Discord | success | 18.6 (windows-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-apple-ios-sim)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097601) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.0 (macos-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097604) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.1 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097616) | Target build, packaging or release contract | Caller verdict/Discord | success | 9.5 (macos-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097618) | Target build, packaging or release contract | Caller verdict/Discord | success | 11.4 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-linux-android)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097619) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.4 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (armv7-linux-androideabi)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097646) | Target build, packaging or release contract | Caller verdict/Discord | success | 9.8 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (x86_64-linux-android)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097674) | Target build, packaging or release contract | Caller verdict/Discord | success | 7.1 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-apple-ios)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223097752) | Target build, packaging or release contract | Caller verdict/Discord | success | 14.4 (macos-latest) |
| [Release Swift Lib / Build wrapper package](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223100948) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.5 (macos-latest) |
| [Docker / Build linux/arm64](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223105293) | Target build, packaging or release contract | Caller verdict/Discord | success | 14.1 (ubuntu-24.04-arm) |
| [Docker / Build linux/amd64](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223105428) | Target build, packaging or release contract | Caller verdict/Discord | success | 15.5 (ubuntu-latest) |
| [Release Py FFI / Build wheels (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223106623) | Target build, packaging or release contract | Caller verdict/Discord | success | 11.5 (ubuntu-24.04-arm) |
| [Release Py FFI / Build sdist](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223106703) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.7 (ubuntu-latest) |
| [Release Py FFI / Build wheels (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223106706) | Target build, packaging or release contract | Caller verdict/Discord | success | 12.3 (ubuntu-latest) |
| [Release Py FFI / Build wheels (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223106720) | Target build, packaging or release contract | Caller verdict/Discord | success | 21.1 (windows-latest) |
| [Release Py FFI / Build wheels (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223107153) | Target build, packaging or release contract | Caller verdict/Discord | success | 10.7 (macos-latest) |
| [Release Kotlin lib / Check version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223111782) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.2 (ubuntu-latest) |
| [platform / Windows](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223136733) | macOS/Windows native coverage | Caller verdict/Discord | success | 47.1 (windows-2022) |
| [platform / macOS](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223136736) | macOS/Windows native coverage | Caller verdict/Discord | success | 39.8 (macos-latest) |
| [Release Dart / Publish moq to pub.dev](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223213928) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Kotlin lib / Publish wrapper](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119226796807) | Target build, packaging or release contract | Caller verdict/Discord | success | 12.4 (ubuntu-latest) |
| [Docker / Publish manifest](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119230819843) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [moq-gst / Release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119233791094) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [moq-c / Release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119234812393) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Swift Lib / Publish dry-run](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119239341314) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.2 (ubuntu-latest) |
| [Release Swift Lib / Verify staged package resolves](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119239341588) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.2 (macos-latest) |
| [moq-relay / release / Release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119239433551) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [moq-cli / release / Release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119240481087) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Dart FFI / Stage Native Assets](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119243120731) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.3 (ubuntu-latest) |
| [Release Dart FFI / Check pub package](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119243120948) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.5 (ubuntu-latest) |
| [Release Dart FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119243122815) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Dart FFI / Attach Native Assets](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119243285817) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Dart FFI / Publish moq_ffi to pub.dev](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119243396361) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Swift Lib / Publish to Swift Package mirror](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119244215431) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Py FFI / Publish to PyPI](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119245884177) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Benchmark site](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119247947573) | Benchmark report completeness/publication | Pages report and failure/Discord | success | 0.9 (ubuntu-24.04-arm) |
| [Release FFI / Shared moq-ffi / Generate bindings (kotlin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119248647987) | Target build, packaging or release contract | Caller verdict/Discord | success | 3.4 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Generate bindings (go)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119248648007) | Target build, packaging or release contract | Caller verdict/Discord | success | 12.7 (ubuntu-latest) |
| [Release FFI / Shared moq-ffi / Generate bindings (swift)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119248648195) | Target build, packaging or release contract | Caller verdict/Discord | success | 3.3 (ubuntu-latest) |
| [Release FFI / C++ native input (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255084196) | Target build, packaging or release contract | Caller verdict/Discord | success | 5.8 (ubuntu-latest) |
| [Release FFI / C++ native input (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255084221) | Target build, packaging or release contract | Caller verdict/Discord | success | 9.1 (windows-latest) |
| [Release FFI / C++ native input (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255084244) | Target build, packaging or release contract | Caller verdict/Discord | success | 4.7 (ubuntu-24.04-arm) |
| [Release FFI / C++ native input (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255084258) | Target build, packaging or release contract | Caller verdict/Discord | success | 7.3 (macos-latest) |
| [Release FFI / Release Swift FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255085195) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [Release FFI / Release Kotlin FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255085212) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.1 (ubuntu-latest) |
| [Release FFI / Release Go FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255085463) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.2 (ubuntu-latest) |
| [Release FFI / Release Swift FFI / Package](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255167202) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.8 (macos-latest) |
| [Release FFI / Release Swift FFI / moq-ffi / Build moq-ffi (${{ matrix.target }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255167754) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Swift FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255168356) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Kotlin FFI / moq-ffi / Build moq-ffi (${{ matrix.target }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255169154) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Kotlin FFI / Package and publish](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255169242) | Target build, packaging or release contract | Caller verdict/Discord | success | 1.4 (ubuntu-latest) |
| [Release FFI / Release Kotlin FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255170075) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Go FFI / moq-ffi / Build moq-ffi (${{ matrix.target }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255205172) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Go FFI / Package](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255205907) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.5 (ubuntu-latest) |
| [Release FFI / Release Go FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255207508) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Go FFI / Publish dry-run](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255480184) | Target build, packaging or release contract | Caller verdict/Discord | success | 0.4 (ubuntu-latest) |
| [Release FFI / Release Go FFI / Publish to Go module mirror](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255482907) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Swift FFI / Attach assets to GitHub release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255586428) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Release Swift FFI / Verify staged package resolves](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255586620) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (macos-latest) |
| [Release FFI / Release Swift FFI / Publish to Swift Package mirror](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119255587813) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release FFI / Publish C++ native inputs](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259717024) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Cpp / Build (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259718011) | Target build, packaging or release contract | Caller verdict/Discord | success | 5.7 (macos-latest) |
| [Release Cpp / Build (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259718018) | Target build, packaging or release contract | Caller verdict/Discord | success | 8.4 (windows-latest) |
| [Release Cpp / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259718067) | Target build, packaging or release contract | Caller verdict/Discord | success | 5.0 (ubuntu-latest) |
| [Release Cpp / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259718283) | Target build, packaging or release contract | Caller verdict/Discord | success | 4.8 (ubuntu-24.04-arm) |
| [Release Cpp / native](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259719527) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (no runner) |
| [Shipping sizes](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119259720028) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-24.04-arm) |
| [Release Cpp / OBS plugin (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119264042068) | Target build, packaging or release contract | Caller verdict/Discord | success | 4.0 (macos-latest) |
| [Release Cpp / OBS plugin (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119264042301) | Target build, packaging or release contract | Caller verdict/Discord | success | 2.5 (windows-2022) |
| [Release Cpp / Release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119264044030) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |
| [Release Cpp / OBS plugin release](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119266176636) | Target build, packaging or release contract | Conditional dry-run skip | skipped | 0.0 (ubuntu-latest) |

#### Every failed job: cause and disposition

| Failed job(s) | Evidence and current disposition |
| --- | --- |
| Six Linux moq-cli, moq-relay and moq-gst packaging jobs | Linker undefined symbols. [6090](https://github.com/moq-dev/moq/pull/6090) merged after this night; needs a new scheduled build for confirmation. |
| Firefox and WebKit media | Firefox audio stayed suspended; WebKit lacked decode capabilities. [6173](https://github.com/moq-dev/moq/pull/6173) merged afterward. Current main removes the WebKit lane; do not re-enable it here. |
| Browser benchmark smoke | `waitForFunction` timed out after browser download DNS failures appeared earlier. [6216](https://github.com/moq-dev/moq/pull/6216) owns browser distribution; the application timeout still needs independent reproduction before calling it fixed. |
| JS Audit and Rust Audit | Advisory findings, including webpki roots. [6169](https://github.com/moq-dev/moq/pull/6169) has since merged; it records upstream-blocked advisories separately. No ignore was added here. |
| Tests (gate) | Unresolved quest link to the old ts-health-stats path, not the R2 warnings earlier in the log. The current quest tree passes. |
| Tests (gh test) | A stale queue fixture expected a removed Smoke pull_request branch filter. Main fixed that fixture. Running the current suite exposed two further stale assertions fixed in this PR. |
| Fuzz (connection) | Assertion `buf_capacity - buf.len() >= MIN_PACKET_SPACE` in connection transmit, crash `5783f2303d2a75af7f12d1798db7b62d651ab54a`. Fixed by [6279](https://github.com/moq-dev/moq/pull/6279) with that input as its regression. |
| Tests (rs loom) | `a_front_sees_a_reader_return` exceeded Loom model branches. The prior parked-reader fix predates this night. The documented current-main Loom rerun was not completed: the preliminary stricter nextest build emitted existing cfg(loom) dead-code warnings and spent over six minutes compiling; the documented recipe then waited on that same worktree build lock. Those redundant validation compilers were stopped, and no model bound was changed. Do not call the historical runtime failure confirmed on current main. |
| Features (rs features) | `moq-mux` fMP4 Rendition and Kind large-enum lints. The existing [lint audit](https://github.com/moq-dev/moq/pull/6159) discusses related feature/platform warnings; it does not establish that these sampled ARM failures are fixed. |
| Rust benchmark tests | `benchmark_feed_fetch_wakes` exceeds the cooperative Ready-poll limit. [6112](https://github.com/moq-dev/moq/pull/6112) and the performance audit track the benchmark; no timing or correctness criterion was weakened here. |
| JS benchmarks | Requests benchmark exits nonzero. Existing performance work owns the numbers; its failure is recorded in the Pages outcome rather than treated as passing. |

The latest scheduled [TS](https://github.com/moq-dev/moq/actions/runs/39607267367)
also failed: missing capture media, T-STD overflows, and a snapshot `moq: old` error.
[6204](https://github.com/moq-dev/moq/pull/6204) addresses the reset-group error,
but does not prove the missing-media or T-STD failures fixed. The TS reliability
quests retain those conditions.

#### Remaining work and decisions

- Treat unavailable exact per-workflow monthly attribution as a measurement limit,
  not a completion gate. Billing totals, measured job costs, invocation counts and
  retained failure links are recorded separately rather than multiplied blindly.
- Replay historical Loom and feature-only lint failures on current
  main before classifying them as current failures, coordinating with existing owners. Protocol or model changes large
  enough to review independently need a maintainer-approved split, as this Plan allows.
- Rust/JS coverage, browser baseline and browser benchmark comparisons now reach
  job summaries while retaining their schedules and assertions. Relay-profile raw
  trace bundles remain diagnosis, not an unconsumed numeric report. Native audio
  quality rows still need the audio-quality owner's reporting/budget decision.
- Confirm post-fix scheduled evidence for Linux release links, browser media, ARM
  audio-device periods and audio budgets. A pass from before a fix is not confirmation.
- Keep the existing capture/play coverage and caching decisions unchanged. Folding
  builds remains a maintainer coverage choice, not an assumed audit optimization.
- Respect concurrent ownership: [6256](https://github.com/moq-dev/moq/pull/6256)
  owns local admission/worker limits, [6247](https://github.com/moq-dev/moq/pull/6247)
  owns binding build cost, [6207](https://github.com/moq-dev/moq/pull/6207) owns
  Pages artifact cleanup, and [6257](https://github.com/moq-dev/moq/pull/6257) owns
  the upstream runner integration and its setup/verdict blocker.
- Finish this run before creating its already-decided m2 recurrence successor. No m2
  quest has been claimed or started by this audit.

#### Connection fuzz crash: fixed

[moq-dev/moq#6279](https://github.com/moq-dev/moq/pull/6279) fixed the
connection transmit panic (`buf_capacity - buf.len() >= MIN_PACKET_SPACE`) with
the exact 847-byte Nightly input (SHA-1 `5783f2303d2a75af7f12d1798db7b62d651ab54a`)
as its regression. The scope decision this audit raised is settled.

#### Local validation and ownership

- Full `nix develop --command just ci` passed before the clean rebase onto
  `bce217e063`. After rebase, quest validation and Markdown checks pass. The final
  `quest gate just ci` waited for host admission without starting CI and was
  cancelled on request; full current-head validation remains pending.

- `just gh test` passes after the two fixture corrections and restored archive
  benchmark coverage. `sh/gh/nightly.test.py` passes all eight tests.
- `just js check` passes; both touched browser projects type-check once the normal
  JS build supplies generated wasm declarations. Browser aggregation tests pass.
- Real `just js coverage clock` and `just rs coverage moq-pattern` runs produce
  the new summaries. The Rust sample reports 748/786 covered lines (95.17%).
- The existing generator report was exercised with all five runner records and
  its Markdown verified in the job summary. No new performance threshold was added.
- [6261](https://github.com/moq-dev/moq/pull/6261) wires its resampler measurement
  into the existing `bench-playback` recipe. It is concurrent owned work, not a
  newly unclaimed benchmark gap in this audit.
- Full browser baseline and benchmark workloads were not rerun for output-only
  presentation edits; existing aggregation tests, project type-checks and JS gate
  cover the edits. The measured workloads retain their Nightly callers.
