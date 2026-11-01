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

This audit owns workflows and jobs, including job-level build cost; the test
cost audit owns individual tests. Each run tables every workflow in
`.github/workflows/` and every job of the latest scheduled Nightly with three
verdicts:

- **Signal:** what regression it catches that nothing cheaper catches, and
  when it last caught one.
- **Visibility:** who sees a failure or a result. A gate blocks merging, a
  scheduled or publishing workflow posts to Discord `#alerts` through
  [alert.yml](/.github/workflows/alert.yml) (`just gh check` enforces its
  coverage), and a report lands on the [Pages site](https://git.moq.dev/) or
  in a job summary. A result only in a run artifact or log is invisible:
  publish it, summarize it, or stop producing it. Each Pages report owns one
  `gh-pages` directory, commits only that directory, deploys the whole branch,
  and joins the `benchmark-pages` concurrency group with `queue: max`
  (decided 2026-10-20; `sh/gh/pages-publish.sh`).
- **Cost:** runner-minutes by runner type, from the org billing API and job
  timings, against that signal. macOS is the scarce one (a 5-job concurrency
  cap).

### Check first (from the m1 run, 2026-10-22)

- Classify every failed job of the latest scheduled Nightly, Interop, and TS
  runs. `gh run view <id> --log-failed` fetches them in one call; job timings
  come from `gh api repos/moq-dev/moq/actions/runs/<id>/jobs`. A fix merged
  after a night is confirmed only by a later night.
- Nightly was red every night of the m1 run. The 2026-10-21 night failed 18
  jobs and the 2026-10-22 night 19 plus a cancelled Loom job. By the end of the
  run, merged fixes covered all but these, which had no owner:
  - Windows only: `reconnect::a_protocol_violation_ends_reconnect`, added by
    [moq-dev/moq#6162](https://github.com/moq-dev/moq/pull/6162), redials
    after a protocol violation over TCP.
  - WASM and the browser backend baseline: on the 2026-10-22 night, an
    uncaught `createUnidirectionalStream` rejection on a closed WebTransport
    session failed the harness although every case passed. Two local runs on
    main `f42b336f0e` did not reproduce it.
- `just test wasm` on main `f42b336f0e` fails `ietf: reads a published
  broadcast` (`remote error: code=0`). Bisected to
  [moq-dev/moq#6348](https://github.com/moq-dev/moq/pull/6348): the harness
  finishes its track before the pre-draft-22 joining FETCH arrives, which
  [join after a quick finish](/quest/m2/ietf-join-after-finish.md) owns. The
  merge queue does not run the browser harness, so only Nightly sees it.
  - Audio quality: two Chromium Opus `wide` rows exceed their `target_ms_p95`
    budget. The [audio jitter target](/quest/m1/audio-jitter-target/README.md)
    owns the budgets.
- Fixed after their night, awaiting a green night: Linux release links
  ([#6090](https://github.com/moq-dev/moq/pull/6090)), Firefox audio
  ([#6321](https://github.com/moq-dev/moq/pull/6321)), macOS Swift SIGBUS
  ([#6323](https://github.com/moq-dev/moq/pull/6323)), runner VM user
  namespaces ([#6324](https://github.com/moq-dev/moq/pull/6324)), minimal
  feature build ([#6312](https://github.com/moq-dev/moq/pull/6312)), CMAF
  decoder tests ([#6313](https://github.com/moq-dev/moq/pull/6313)), relay
  drain ([#6322](https://github.com/moq-dev/moq/pull/6322)), the impaired
  cluster drill ([#6314](https://github.com/moq-dev/moq/pull/6314)), the
  browser benchmark build ([#6316](https://github.com/moq-dev/moq/pull/6316)),
  the supplemental Criterion list
  ([#6326](https://github.com/moq-dev/moq/pull/6326)), setup and feature
  fixtures ([#6325](https://github.com/moq-dev/moq/pull/6325)), the
  connection fuzz crash ([#6279](https://github.com/moq-dev/moq/pull/6279)),
  and the JS and Rust advisories
  ([#6315](https://github.com/moq-dev/moq/pull/6315),
  [#6317](https://github.com/moq-dev/moq/pull/6317)). The moq-mux large-enum
  and moq-net minimal-feature lints pass on main `f42b336f0e`.
- Interop's finite-track tails fail before draft 22 (`Error: old`, a JS
  `RESET_STREAM`); [join after a quick finish](/quest/m2/ietf-join-after-finish.md)
  owns them. TS fails T-STD overflow and late audio starts; the TS reliability
  quests own them. The relay-log `FAIL` lines in Interop's self-test are
  negative controls, not failures.
- Infrastructure flakes in the sample: DNS failures installing Nix
  (`cli-export`) and a curl exit 56 downloading BBB. Count them across nights
  before acting; one night is not a pattern.
- `just gh test` ran only in Nightly during the m1 run, so a PR that added a
  bench without registering it, or changed a workflow a fixture asserts,
  merged green and reddened the next night. That happened three times in two
  days (the setup fixture, the removed `writer` bench, and the unregistered
  jitter and SUBSCRIBE_TRACKS benches the m1 run registered).
  [moq-dev/moq#6480](https://github.com/moq-dev/moq/pull/6480) moved it into
  every Check; confirm no such break recurs.
- Platform (macOS, Windows) now runs only in Nightly and on release PRs, not
  in the merge queue. A Windows- or macOS-only regression surfaces first in
  Nightly, as the reconnect test did.
- An upload that requires files must wait for its producing step, or a setup
  failure adds a misleading "No files were found" error. The m1 run gated the
  Platform, x11, and size uploads; `failure()`-gated diagnostics still assume
  their run directory exists.
- A queue failure often belongs to another PR in the same batch. Check the
  entry's head and what merged around it before calling it a flake.
- Re-running a single job cannot redeploy Pages: the earlier attempt's
  `github-pages` artifact remains and `deploy-pages` refuses two. Delete the
  stale artifact first, or wait for the next Nightly.
- Deferred to the maintainer since m0 (coverage trade-off): fold the capture
  and play test builds into the main test build (about 75 s per core Rust
  queue entry) or move both to Nightly. The queue keeps both builds until
  decided.
- Owned elsewhere: Rust and JS advisories by the
  [dependency audit](/quest/m2/audit-deps.md); benchmark legs by the
  performance audit; binding build cost, local admission, and Pages artifact
  cleanup by their own quests.

### Baselines

- Org billing for this repository, October through 2026-10-22 UTC: Linux
  116,552 minutes, Linux ARM 221,937, macOS 54,829, Windows 52,651, all at
  zero net charge. The per-workflow and per-run billing endpoints return
  nothing for public runs, so attribute cost from job timings, never by
  multiplying invocation counts by one sample.
- Nightly elapsed runner-minutes: 1,372 on 2026-10-21 (ARM 636, Linux 260,
  macOS 290, Windows 186; 166 jobs, much of it cut short by early failures) and
  2,091 on 2026-10-22 (ARM 1,328, Linux 335, macOS 273, Windows 156; 143 jobs
  ran). The longest 2026-10-22 jobs: Audio quality 62 minutes, Loom 60
  (cancelled), binding benchmark 52, Rust bench targets 48, relay profiles 45,
  Rust benchmark tests 44, the gate suite 40.
- The merge queue runs one `just ci` job on the self-hosted `moq-queue` runner:
  median 5.5 minutes, p90 11.9, at most 24.2 over 32 entries (m0, 2026-10-21).
  Its job summary lists seconds per module, and the `cargo-timings` artifact
  is the per-crate drill-down.
- Job summaries now carry Rust and JS coverage, the browser backend baseline,
  and browser benchmark comparisons (m1). Relay-profile traces stay
  artifact-only diagnosis; native audio rows
  wait on the audio jitter target's reporting decision.

### Every workflow (m1, 2026-10-22)

Invocations count October runs, including skipped ones, capped by GitHub at
2,500. Minutes are the latest direct run's jobs, by runner. A success is
coverage evidence, not a regression caught.

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
| [interop](/.github/workflows/interop.yml) | Cross-language protocol/media behavior | Discord and Pages interop report | 743 | Linux 21.6 | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39787655240) |
| [moq-c](/.github/workflows/moq-c.yml) | C ABI packaging, fixtures and consumer links | Caller gate/alert and release artifacts | 0 | no executed job sample | No retained standalone failure |
| [moq-cli](/.github/workflows/moq-cli.yml) | CLI release archives and target-specific links | Caller gate/alert and release artifacts | 8 | ARM 6.5, Linux 6.2, Windows 17.1, macOS 2.6 | [2026-10-16](https://github.com/moq-dev/moq/actions/runs/38854842213) |
| [moq-gst](/.github/workflows/moq-gst.yml) | GStreamer plugin packaging and dynamic links | Caller gate/alert and release artifacts | 11 | ARM 9.4, Linux 10.3, macOS 10.8 | [2026-10-17](https://github.com/moq-dev/moq/actions/runs/39000627149) |
| [moq-relay](/.github/workflows/moq-relay.yml) | Relay release archives and target-specific links | Caller gate/alert and release artifacts | 8 | ARM 5.1, Linux 5.3, Windows 9.3, macOS 44.1 | [2026-09-30](https://github.com/moq-dev/moq/actions/runs/36747636505) |
| [moq-sh](/.github/workflows/moq-sh.yml) | Installer publication, fixtures and installed artifact smoke | Published repository/installer and alert | 13 | Linux 0.2 | No retained standalone failure |
| [nightly](/.github/workflows/nightly.yml) | Diff-independent regression suite and release dry-runs | Discord, Pages benchmarks, job summaries | 26 | see Baselines | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39779729461) |
| [obs](/.github/workflows/obs.yml) | OBS consumer build and plugin packaging | Caller gate and Nightly alert | 2103 | ARM 22.2, Windows 12.9, macOS 10.9 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39343452410) |
| [oss-scanner](/.github/workflows/oss-scanner.yml) | OSS Scanner container builds | Caller gate and Nightly alert | 1 | ARM 9.7 | No retained standalone failure |
| [platform](/.github/workflows/platform.yml) | macOS/Windows compile, bindings and native tests | Nightly and release-gate alert | at least 2,500 | Windows 12.3, macOS 7.0 | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39779729461) |
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
| [runner-vm](/.github/workflows/runner-vm.yml) | Persistent runner VM configuration and boot/image checks | Caller gate and Nightly alert | 4 | Linux 3.2 | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39779729461) |
| [smoke-toolchain](/.github/workflows/smoke-toolchain.yml) | Coupled smoke tooling and Playwright update validation | Update PR and scheduled failure alert | 0 | no executed job sample | No retained standalone failure |
| [smoke](/.github/workflows/smoke.yml) | Published distribution and external-relay behavior | Discord and Pages relay report | 21 | Linux 95.6 | [2026-09-25](https://github.com/moq-dev/moq/actions/runs/36076458401) |
| [ts](/.github/workflows/ts.yml) | TS container compliance and round-trip media integrity | Scheduled failure alert and release gate | 1493 | Linux 34.9 | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39786183107) |
| [uring](/.github/workflows/uring.yml) | io_uring compile and runtime/kernel contract | Caller gate and Nightly alert | 1711 | ARM 7.7 | [2026-10-19](https://github.com/moq-dev/moq/actions/runs/39344299025) |
| [video](/.github/workflows/video.yml) | Browser video renderer behavior | Scheduled failure alert and release gate | 413 | Linux 1.9 | No retained standalone failure |
| [wasm](/.github/workflows/wasm.yml) | Native/wasm cross-language bindings and browser behavior | Nightly and release-gate alert | at least 2,500 | ARM 23.8 | [2026-10-22](https://github.com/moq-dev/moq/actions/runs/39779729461) |
| [x11](/.github/workflows/x11.yml) | SHM and GetImage native capture paths | Caller gate and Nightly alert | 680 | ARM 5.2 | [2026-10-17](https://github.com/moq-dev/moq/actions/runs/39002028489) |

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
