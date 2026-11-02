# [M] CI workflow audit

## Goal

Every CI workflow and job earns its place: its results reach someone (a gate,
a Discord alert, or a published report) at a cost proportional to that signal.
Each one that doesn't is deleted by default, or else made cheaper or moved. Individual tests stay with
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

Default to removing (decided 2026-11-01, maintainer): whatever doesn't earn its
cost is deleted inline, and the PR body lists each deletion with its measured
cost and why it wasn't worth it. Only what earns its keep is refactored to be
cheaper instead. Removing a public API or a user-facing feature stays a
follow-up.

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

### Check first (off-cycle m1 audit, 2026-11-01)

- The latest scheduled [Nightly](https://github.com/moq-dev/moq/actions/runs/41313466339)
  is still red: 152 jobs executed, 19 failed and one was cancelled; 28 tag-only
  publishers or reused builders were skipped. Read failures by cause, not job count.
  A green compile or an alert delivery is not proof of a caught product regression.
- New signal worth retaining on November 1: QUIC connection fuzzing found
  `corrupt stream data` at `rs/moq-quic/src/fuzz/connection.rs:835`; the resume
  model found missing group 0 after widening an unordered reader's range.
  Gate, all-features and Windows exposed the relay stack test failure, already
  owned by [moq-dev/moq#7595](https://github.com/moq-dev/moq/pull/7595).
  These are reasons to keep the lanes, not reasons to disable their tests.
- Eight moq-net fuzz targets (announce, ietf, lite, pattern, ingest, varint,
  request and path) failed to build with Rust E0391 while computing async drop
  for `model::bandwidth::Consumer`; they did not run fuzzing. The six other
  fuzz jobs ran, including the QUIC corruption above. Do not count a build
  failure as eight independent fuzz findings.
- Other November 1 failures: Rust audit reports RUSTSEC-2026-0173; binding
  benchmarks fail `benchmark_ffi`; Firefox media fails the codec-rename tone
  check (69/78 samples); shared benches confirm six paired slowdowns;
  supplemental benches lack 15 base comparisons after the baseline lacks
  `moq-sign`; relay session benches panic with `Evicted`; Rust bench targets
  reach measurement but are cancelled at 60.4 minutes. Confirm calibration
  before calling paired slowdowns product regressions: [bench CI](/quest/m1/bench-ci.md)
  owns it. [moq-dev/moq#7592](https://github.com/moq-dev/moq/pull/7592) already
  verifies that the old paired-base Cargo alias blocker is past.
- Latest scheduled [Interop](https://github.com/moq-dev/moq/actions/runs/41317901057)
  costs 48.2 Linux minutes and fails buffering, a catalog assertion, and the
  removed `moq-transport-14` version. [moq-dev/moq#7553](https://github.com/moq-dev/moq/pull/7553)
  owns the join-test version matrix. The relay-log self-test's empty, timeout
  and pinned-round `FAIL` messages are intentional negative controls.
  [TS](https://github.com/moq-dev/moq/actions/runs/41317281352) passes in 42.0
  Linux minutes. [Video](https://github.com/moq-dev/moq/actions/runs/41318496744)
  fails an assertion in 7.8 Linux minutes; malformed attribute diagnostics
  before it are expected test inputs. These tests stay with their owners.
- Visibility gaps retained with existing owners: Rust benchmark tests and
  shipping sizes are measured but not on Pages yet;
  [moq-dev/moq#7510](https://github.com/moq-dev/moq/pull/7510) publishes both.
  Binding runner checks have a current consumer in rust2's pinned comparison;
  they smoke-run here rather than claiming cross-night timing evidence.
  Native audio rows are still non-enforced and artifact-only; the
  [audio jitter target](/quest/m1/audio-jitter-target/README.md) owns their
  reporting/budget decision. Do not delete that scope in a workflow cleanup.
- Concrete overlap: [moq-dev/moq#7600](https://github.com/moq-dev/moq/pull/7600)
  owns test deletions; [moq-dev/moq#7601](https://github.com/moq-dev/moq/pull/7601)
  owns dependencies; [moq-dev/moq#7602](https://github.com/moq-dev/moq/pull/7602)
  owns source, generics and generated-code compile cost;
  [moq-dev/moq#7598](https://github.com/moq-dev/moq/pull/7598) owns dispatcher
  KVM testability; [moq-dev/moq#7593](https://github.com/moq-dev/moq/pull/7593)
  owns io_uring grouping/diagnostics; [moq-dev/moq#7541](https://github.com/moq-dev/moq/pull/7541)
  owns JS bench reachability. No infrastructure, cache or landing-gate work
  is duplicated here. In particular, cache 5xx belongs to
  [moq-dev/moq#7537](https://github.com/moq-dev/moq/pull/7537), and publisher
  environments to [moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431).
- Local full checks can hit upstream mbx E2BIG
  ([moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594)) and the runner-image
  `/proc/self/uid_map` denial already reproduced on main. Neither justifies
  bypassing the launcher, skipping a selected module, or claiming a full pass.
- Keep release target/artifact removal as a maintainer decision. Shared
  default-feature FFI builds already feed Go/Kotlin/Swift/C/C++; Dart has a
  different target set and packaging contract. Do not count their matching
  target names as evidence of redundant outputs.

### Baselines

Timings below are elapsed runner-minutes, summed from every job's
`started_at`/`completed_at` in the REST jobs API, excluding skipped jobs.
They include setup, builds and teardown, not just workloads. They are neither
CPU-minutes nor billed rounded minutes, and overlapping workflow/caller rows
must not be added together. All observations were fetched November 1 local
(November 2 UTC); no workflow was dispatched for this audit.

- October org billing for `moq`: Linux 309,940 minutes, Linux ARM 243,405,
  macOS 62,756 and Windows 55,972; net charge $0. Source:
  `GET /organizations/moq-dev/settings/billing/usage?year=2026&month=10`.
  This is monthly attribution only. Do not infer a workflow's cost from it,
  multiply a sample by invocation count, or confuse $0 with free capacity.
- Nightly on October 31: 1,402.1 minutes (ARM 767.0, Linux 292.2,
  macOS 218.1, Windows 124.8). November 1: 1,505.7 (ARM 780.1,
  Linux 303.6, macOS 282.1, Windows 139.9). Early failures make these
  lower bounds on a fully successful run, not a performance trend.
- No hosted merge-queue job exists now. Main PRs skip Check, and
  `quest land serve` runs `just ci` on the maintainer's host. The prior hosted
  queue's 5.5-minute median is obsolete; this audit claims no replacement
  host baseline. Release PRs still run the full Check and Release gate.
- Removed from `nightly.yml`: audio grouping, shard sweep and relay profile
  measurement jobs. Across all 13 scheduled nights, October 20 to November 1,
  every one succeeded and no caught regression was found. They have no
  comparison gate, summary or published series; successful numbers remain in
  logs and the profile captures disappear with the runner. Local commands
  stay useful for a specific investigation. Profile tooling's deterministic
  tests still run through `gh test`; the scheduled capture smoke is dropped.
  The jobs' actual cost, including setup, is:

| Deleted Nightly job | Runner | 13-night minutes | November 1 minutes | Verdict |
| --- | --- | ---: | ---: | --- |
| Audio grouping relay benchmark | ARM | 200.5 | 13.8 | Delete scheduled measurement; keep on-demand diagnostic |
| Relay profiles | Linux | 356.4 | 29.6 | Delete scheduled measurement; keep on-demand diagnostic |
| Relay shard sweep | ARM | 190.2 | 12.1 | Delete scheduled measurement; keep on-demand diagnostic |

The November 1 removed jobs total 55.5 runner-minutes (25.9 ARM, 29.6 Linux),
not a measured reduction in Nightly wall time. The 13-night total is 747.2
runner-minutes. This is the only workflow changed in this off-cycle run.
The recurring audit and its release prerequisite remain; no successor is made.

### Every workflow (2026-11-01)

Every checked-in workflow is listed below. `N` costs are its nested jobs in
November 1 Nightly; `D` costs are the latest completed successful/failed direct
run in the fetched page (up to 100 runs since October 20). A reusable wrapper with no direct run is not zero cost:
its caller pays. Failure dates below are observations, not automatically unique
regressions. For retained lanes without an attributed regression since October
20, the signal column states the distinct boundary that earns their cost.

All scheduled/publishing failures reach Discord through Alert; direct PR/push
validation reaches its GitHub check, not Discord. Pages or job summaries are
called out where they expose successful measurements. Artifacts are diagnosis,
not a visible report. Main branch cache writes are protected by `cache-write`;
release credentials remain under the existing publisher-environment work above.

| Workflow | Unique signal / verdict | Result visibility | Sample runner-minutes |
| --- | --- | --- | --- |
| [alert](/.github/workflows/alert.yml) | Keep: delivers failures; coverage/delivery fixtures exercise the contract | Discord; coverage gate | [D 2026-11-01](https://github.com/moq-dev/moq/actions/runs/41322009598) Linux 0.1 |
| [android](/.github/workflows/android.yml) | Keep: Android cross-compilation and packaging beyond the native gate | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 3.4 |
| [api-diff](/.github/workflows/api-diff.yml) | Keep: release backport API compatibility, distinct from compilation | Release gate, summary/report | No executed direct sample since October 20; cost belongs to caller |
| [apt-repo](/.github/workflows/apt-repo.yml) | Keep: APT repository metadata, signatures and publication | Published repository/installer and alert | [D 2026-10-30](https://github.com/moq-dev/moq/actions/runs/41042492573) Linux 1.7 |
| [audio-quality](/.github/workflows/audio-quality.yml) | Keep: deterministic audio replay correctness; real-time quality is in Nightly | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 4.5 |
| [c](/.github/workflows/c.yml) | Keep: generated C bindings and installed-consumer ABI link | Caller gate / Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 4.5 |
| [cache](/.github/workflows/cache.yml) | Keep, infrastructure-owned: publishes reusable shells on three hosts | GitHub status; no product regression claim | [D 2026-11-02](https://github.com/moq-dev/moq/actions/runs/41377533846) ARM 1.3, Linux 1.3, macOS 3.5 |
| [check](/.github/workflows/check.yml) | Keep: release admission; main admission runs locally through quest land | Required Check; module timing summary | No completed direct sample in fetched page |
| [codspeed](/.github/workflows/codspeed.yml) | Keep: instruction counts separate layout/timing noise; explicit October 28 maintainer decision in bench-ci | CodSpeed dashboard; failure alert | [D 2026-11-02](https://github.com/moq-dev/moq/actions/runs/41368239240) Linux 63.2 |
| [cpp](/.github/workflows/cpp.yml) | Keep: C++ compiler and ABI consumer builds; not only Rust compilation | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 6.3 |
| [dependabot](/.github/workflows/dependabot.yml) | Keep: dependency PR policy and maintenance automation | Dependency PR state | [D 2026-11-02](https://github.com/moq-dev/moq/actions/runs/41372931202) Linux 0.6 |
| [docker](/.github/workflows/docker.yml) | Keep: container image build and multi-architecture publication | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 13.1, Linux 15.8 |
| [features](/.github/workflows/features.yml) | Keep: feature isolation and missing dependency gates | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 53.1 |
| [ietf-conformance](/.github/workflows/ietf-conformance.yml) | Keep: pinned third-party Moxygen protocol behavior | Nightly failure alert; reports are diagnostics | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 29.8 |
| [interop](/.github/workflows/interop.yml) | Keep: cross-language protocol/media behavior | Discord and Pages interop report | [D 2026-11-01](https://github.com/moq-dev/moq/actions/runs/41317901057) Linux 48.2 |
| [moq-c](/.github/workflows/moq-c.yml) | Keep: C ABI packaging, fixtures and consumer links | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 10.4, Linux 12.2, Windows 14.9, macOS 23.7 |
| [moq-cli](/.github/workflows/moq-cli.yml) | Keep: CLI release archives and target-specific links | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 11.5, Linux 12.4, Windows 21.1, macOS 38.1 |
| [moq-gst](/.github/workflows/moq-gst.yml) | Keep: GStreamer plugin packaging and dynamic links | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 12.2, Linux 15.6, macOS 21.7 |
| [moq-relay](/.github/workflows/moq-relay.yml) | Keep: relay release archives and target-specific links | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 6.6, Linux 7.5, Windows 12.0, macOS 15.0 |
| [moq-sh](/.github/workflows/moq-sh.yml) | Keep: installer publication, fixtures and installed artifact smoke | Published repository/installer and alert | No executed direct sample since October 20; cost belongs to caller |
| [mutants](/.github/workflows/mutants.yml) | Keep: security/grant/resume/expiry test sensitivity, distinct from rerunning tests; newly added, no scheduled sample yet | Survivors in summary and failure alert | No completed direct sample in fetched page |
| [nightly](/.github/workflows/nightly.yml) | Delete three log-only relay measurement jobs; keep diff-independent coverage below | Discord, Pages, job summaries | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 780.1, Linux 303.6, Windows 139.9, macOS 282.1 |
| [obs](/.github/workflows/obs.yml) | Keep: OBS consumer build and plugin packaging | Caller gate and Nightly alert | [D 2026-10-25](https://github.com/moq-dev/moq/actions/runs/40211940636) ARM 20.4, Windows 12.2, macOS 10.6 |
| [oss-scanner](/.github/workflows/oss-scanner.yml) | Keep: OSS Scanner container builds | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 9.2 |
| [platform](/.github/workflows/platform.yml) | Keep: macOS/Windows cfg, ABI, Swift and native test boundaries | Nightly/release gate alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 0.2, Windows 18.2, macOS 57.5 |
| [pullfrog](/.github/workflows/pullfrog.yml) | Keep: explicitly dispatched agent work, no standing regression cost | Calling agent result / GitHub posts | [D 2026-11-02](https://github.com/moq-dev/moq/actions/runs/41378234938) Linux 3.5 |
| [quest](/.github/workflows/quest.yml) | Keep: validates direct pushes after quest-tree merges | GitHub Quest status; no Discord | [D 2026-11-02](https://github.com/moq-dev/moq/actions/runs/41378565460) ARM 0.4 |
| [release-binary](/.github/workflows/release-binary.yml) | Keep: shared target-specific binary packaging | Calling release workflow; not independently triggered | No executed direct sample since October 20; cost belongs to caller |
| [release-brew](/.github/workflows/release-brew.yml) | Keep: Homebrew publication | Caller gate/alert and release artifacts | [D 2026-10-30](https://github.com/moq-dev/moq/actions/runs/41042504830) Linux 0.3 |
| [release-c](/.github/workflows/release-c.yml) | Keep: generated C package and installed consumers over validated native inputs | Caller gate / alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 3.0, Linux 2.6, Windows 7.5, macOS 4.6 |
| [release-cpp](/.github/workflows/release-cpp.yml) | Keep: C++ release artifacts and consumers | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 3.3, Linux 3.8, Windows 12.5, macOS 9.1 |
| [release-dart-ffi](/.github/workflows/release-dart-ffi.yml) | Keep: Dart native FFI artifacts | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 41.2, Windows 17.3, macOS 57.7 |
| [release-dart](/.github/workflows/release-dart.yml) | Keep: Dart package publication | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 0.4 |
| [release-ffi](/.github/workflows/release-ffi.yml) | Keep: shared FFI target builder | Calling release workflow; not independently triggered | No executed direct sample since October 20; cost belongs to caller |
| [release-gate](/.github/workflows/release-gate.yml) | Keep: complete release PR admission, including callable platform lanes | Caller gate/alert and release artifacts | [D 2026-11-02](https://github.com/moq-dev/moq/actions/runs/41376436029) ARM 0.2, Windows 6.5, macOS 4.4 |
| [release-go-ffi](/.github/workflows/release-go-ffi.yml) | Keep: Go native FFI artifacts | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 1.0 |
| [release-go](/.github/workflows/release-go.yml) | Keep: Go wrapper publication | Caller gate/alert and release artifacts | [D 2026-10-31](https://github.com/moq-dev/moq/actions/runs/41182420464) Linux 1.4 |
| [release-js](/.github/workflows/release-js.yml) | Keep: npm/JSR packages and exports | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 6.0 |
| [release-kt-ffi](/.github/workflows/release-kt-ffi.yml) | Keep: Kotlin native FFI artifacts | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 1.7 |
| [release-kt-lib](/.github/workflows/release-kt-lib.yml) | Keep: Kotlin wrapper artifacts | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 11.1 |
| [release-nix](/.github/workflows/release-nix.yml) | Keep: Nix release evaluation/publication | Caller gate/alert and release artifacts | No completed direct sample in fetched page |
| [release-py-ffi](/.github/workflows/release-py-ffi.yml) | Keep: Python native wheels | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 11.2, Linux 19.5, Windows 13.8, macOS 8.9 |
| [release-py](/.github/workflows/release-py.yml) | Keep: Python wrapper package | Caller gate/alert and release artifacts | [D 2026-10-31](https://github.com/moq-dev/moq/actions/runs/41182392402) Linux 17.6 |
| [release-rs](/.github/workflows/release-rs.yml) | Keep: Rust package release policy | Caller gate/alert and release artifacts | [D 2026-10-31](https://github.com/moq-dev/moq/actions/runs/41182420560) Linux 22.4 |
| [release-shared-ffi](/.github/workflows/release-shared-ffi.yml) | Keep: one native build per shared target; packages consume these outputs | Caller gate / alert and release artifacts | [N caller total](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 3.7, Linux 74.6, Windows 22.5, macOS 44.9 (includes nested Go/Kotlin/Swift packagers above) |
| [release-swift-ffi](/.github/workflows/release-swift-ffi.yml) | Keep: Swift native FFI artifacts | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 0.2, macOS 1.0 |
| [release-swift-lib](/.github/workflows/release-swift-lib.yml) | Keep: Swift wrapper publication | Caller gate/alert and release artifacts | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 0.3, macOS 0.9 |
| [release-winget](/.github/workflows/release-winget.yml) | Keep: winget manifests and publication | Caller gate/alert and release artifacts | [D 2026-10-30](https://github.com/moq-dev/moq/actions/runs/41041265123) Windows 0.4 |
| [rpm-repo](/.github/workflows/rpm-repo.yml) | Keep: RPM repository metadata/signatures/publication | Published repository/installer and alert | [D 2026-10-30](https://github.com/moq-dev/moq/actions/runs/41042495150) Linux 2.4 |
| [runner-vm](/.github/workflows/runner-vm.yml) | Keep: persistent runner VM configuration and boot/image checks | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) Linux 7.1 |
| [smoke-toolchain](/.github/workflows/smoke-toolchain.yml) | Keep: coupled smoke tooling and Playwright update validation | Update PR and scheduled failure alert | [D 2026-10-26](https://github.com/moq-dev/moq/actions/runs/40353286871) Linux 1.3 |
| [smoke](/.github/workflows/smoke.yml) | Keep: published distribution and external-relay behavior | Discord and Pages relay report | [D 2026-11-01](https://github.com/moq-dev/moq/actions/runs/41325786483) Linux 55.6 |
| [ts](/.github/workflows/ts.yml) | Keep: TS container compliance and round-trip media integrity | Scheduled failure alert and release gate | [D 2026-11-01](https://github.com/moq-dev/moq/actions/runs/41317281352) Linux 42.0 |
| [uring](/.github/workflows/uring.yml) | Keep: io_uring compile and runtime/kernel contract | Caller gate and Nightly alert | [D 2026-10-20](https://github.com/moq-dev/moq/actions/runs/39372116327) ARM 7.7 |
| [video](/.github/workflows/video.yml) | Keep: browser video renderer behavior | Scheduled failure alert and release gate | [D 2026-11-01](https://github.com/moq-dev/moq/actions/runs/41318496744) Linux 7.8 |
| [x11](/.github/workflows/x11.yml) | Keep: SHM and GetImage native capture paths | Caller gate and Nightly alert | [N](https://github.com/moq-dev/moq/actions/runs/41313466339) ARM 6.2 |

### Every job of November 1 scheduled Nightly

These are the actual run's jobs, including skipped tag-only work. A retained
job's failure is delivered through the parent Nightly alert; `Pages` and
`summary` add successful-result visibility. Package/build rows are kept for
their workflow's distinct target/ABI/package contract above, not because each
successful target has demonstrated a regression. Skipped jobs cost zero and
are not extra nightly work. `Delete` is the decision applied by this PR;
all other rows are retained, with exceptions/owners stated below and above.

| Job | Runner minutes | Result | Signal / visibility verdict |
| --- | ---: | --- | --- |
| [Audio quality](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517677) | ARM 42.0 | success | Real-time impaired audio budgets / alert; native reporting decision owned elsewhere |
| [Benchmark site](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124885762600) | ARM 0.7 | success | Pages delivery, not another measurement |
| [Docker / Build linux/amd64](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868567167) | Linux 15.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Docker / Build linux/arm64](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868567176) | ARM 13.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Docker / Parse tag](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517747) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Docker / Publish manifest](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124873286713) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Fuzz (announce)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517606) | ARM 3.2 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (connection)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517730) | ARM 3.9 | failure | Fresh ASan fuzz inputs; corrupt stream data caught |
| [Fuzz (frame)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517952) | ARM 8.3 | success | Fresh ASan fuzz inputs; crash alert/corpus |
| [Fuzz (ietf)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517805) | ARM 3.1 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (ingest)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517897) | ARM 3.2 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (lite)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517868) | ARM 3.1 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (packet)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518136) | ARM 8.4 | success | Fresh ASan fuzz inputs; crash alert/corpus |
| [Fuzz (params)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517933) | ARM 8.2 | success | Fresh ASan fuzz inputs; crash alert/corpus |
| [Fuzz (path)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518085) | ARM 3.0 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (pattern)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517876) | ARM 3.0 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (request)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517993) | ARM 3.2 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Fuzz (streamid)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517890) | ARM 8.4 | success | Fresh ASan fuzz inputs; crash alert/corpus |
| [Fuzz (streams)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517961) | ARM 8.2 | success | Fresh ASan fuzz inputs; crash alert/corpus |
| [Fuzz (varint)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517927) | ARM 3.1 | failure | Fresh ASan fuzz inputs; E0391 build failure, no fuzzing |
| [Media (firefox player)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517779) | ARM 11.8 | failure | Firefox decode/lifecycle beyond Chromium / codec-rename alert |
| [Nightly (Audio grouping relay benchmark)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518130) | ARM 13.8 | success | Delete: no regression gate or visible measurement |
| [Nightly (Audit)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517879) | ARM 1.2 | failure | Time-dependent Rust advisory; RUSTSEC-2026-0173 alert |
| [Nightly (BBB rendition alignment)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518050) | ARM 1.3 | success | Published fixture rendition alignment; correctness alert |
| [Nightly (Binding benchmark runners)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517685) | ARM 37.4 | failure | Binding-runner contract used by rust2; correctness alert |
| [Nightly (Browser benchmark smoke)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517775) | ARM 15.6 | success | Browser benchmark harness/output validity; correctness alert |
| [Nightly (Go audit)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518270) | ARM 5.0 | success | Time-dependent Go advisories; alert |
| [Nightly (JS audit)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517806) | ARM 1.1 | success | Time-dependent JS advisories; alert |
| [Nightly (JS benchmarks)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517631) | ARM 9.8 | success | JS benchmark smoke / Pages series |
| [Nightly (JS coverage)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517764) | ARM 7.0 | success | Instrumented JS test reachability / summary |
| [Nightly (OBS TSan)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517771) | ARM 9.2 | success | Native consumer data races beyond ordinary tests; alert |
| [Nightly (OBS)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517684) | ARM 9.0 | success | External staticlib/native-link consumer; alert |
| [Nightly (Python audit)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517713) | ARM 1.1 | success | Time-dependent Python advisories; alert |
| [Nightly (Relay profiles)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518097) | Linux 29.6 | success | Delete: no regression gate or visible measurement |
| [Nightly (Relay session benchmarks)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517800) | ARM 14.3 | failure | Relay-specific Criterion path / paired gate and Pages; Evicted |
| [Nightly (Relay shard sweep)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517871) | ARM 12.1 | success | Delete: no regression gate or visible measurement |
| [Nightly (Rust bench targets)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518082) | ARM 60.4 | cancelled | Criterion coverage / paired gate and Pages; calibration owner |
| [Nightly (Rust benchmark tests)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518054) | ARM 35.0 | success | Benchmark executable correctness; reporting owned by 7510 |
| [Nightly (Rust coverage)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517751) | ARM 18.8 | success | Instrumented Rust test reachability / summary |
| [Nightly (Rust shared bench targets)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517638) | ARM 46.7 | failure | Shared Criterion targets / paired gate and Pages; six slowdowns |
| [Nightly (Rust supplemental benchmarks)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517667) | ARM 14.9 | failure | Supplemental Criterion targets / paired gate and Pages; missing base |
| [Nightly (io_uring benchmarks)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517673) | ARM 14.6 | success | io_uring-specific Criterion path / paired gate and Pages |
| [Nightly (moq-c C fixtures)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517716) | ARM 5.0 | success | C ABI link/fixtures outside cargo; alert |
| [Release C / Build (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867000) | macOS 4.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release C / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867021) | ARM 3.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release C / Build (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881866901) | Windows 7.5 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release C / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881866933) | Linux 2.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release C / Release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124883708893) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Cpp / Build (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867271) | macOS 5.3 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Cpp / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867311) | ARM 3.3 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Cpp / Build (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867285) | Windows 8.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Cpp / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867267) | Linux 3.8 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Cpp / OBS plugin (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124883956275) | macOS 3.8 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Cpp / OBS plugin (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124883956221) | Windows 3.9 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Cpp / OBS plugin release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124884924222) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Cpp / Release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124883956930) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Cpp / native](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881868324) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Dart / Check package](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518572) | Linux 0.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart / Publish moq to pub.dev](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868622624) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Dart FFI / Attach Native Assets](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881135590) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Dart FFI / Check pub package](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881048718) | Linux 0.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / Publish moq_ffi to pub.dev](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881152175) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Dart FFI / Stage Native Assets](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881048709) | Linux 0.3 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566148) | macOS 14.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-apple-ios)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566100) | macOS 11.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-apple-ios-sim)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566160) | macOS 11.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-linux-android)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566093) | Linux 6.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566120) | Linux 8.9 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (armv7-linux-androideabi)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566149) | Linux 8.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566281) | macOS 9.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-apple-ios)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566135) | macOS 11.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-linux-android)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566104) | Linux 8.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566189) | Windows 17.3 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Build moq-ffi (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868566158) | Linux 9.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Dart FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881049144) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Dart FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518655) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / C++ native input (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723377) | macOS 5.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / C++ native input (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723373) | ARM 3.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / C++ native input (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723423) | Windows 8.8 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / C++ native input (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723386) | Linux 3.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Publish C++ native inputs](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881867604) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Go FFI / Package](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879765318) | Linux 0.5 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Release Go FFI / Publish dry-run](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879882907) | Linux 0.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Release Go FFI / Publish to Go module mirror](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879883877) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Go FFI / moq-ffi / Build moq-ffi (${{ matrix.target }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879765704) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Go FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879766102) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Go FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723881) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Release Kotlin FFI / Package and publish](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879775722) | Linux 1.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Release Kotlin FFI / moq-ffi / Build moq-ffi (${{ matrix.target }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879776434) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Kotlin FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879776251) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Kotlin FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723767) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Release Swift FFI / Attach assets to GitHub release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124880491128) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Swift FFI / Package](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879776245) | macOS 1.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Release Swift FFI / Publish to Swift Package mirror](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124880491888) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Swift FFI / Verify staged package resolves](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124880491544) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Swift FFI / moq-ffi / Build moq-ffi (${{ matrix.target }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879776532) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Swift FFI / moq-ffi / Generate bindings (${{ matrix.language }})](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879777518) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release FFI / Release Swift FFI / moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879723777) | Linux 0.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556663) | macOS 15.8 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-apple-ios)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556690) | macOS 12.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-apple-ios-sim)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556677) | macOS 10.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-linux-android)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556704) | Linux 8.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556694) | Linux 8.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (armv7-linux-androideabi)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556675) | Linux 6.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (x86_64-linux-android)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556731) | Linux 8.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556684) | Windows 13.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Build moq-ffi (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868556712) | Linux 11.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Generate bindings (go)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124875369657) | Linux 17.5 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Generate bindings (kotlin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124875369587) | Linux 3.5 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Generate bindings (swift)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124875369588) | Linux 3.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release FFI / Shared moq-ffi / Parse version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518619) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release JS / Release JS Packages](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518542) | Linux 6.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Kotlin lib / Check FFI publisher](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517704) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Kotlin lib / Check version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868567036) | Linux 0.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Kotlin lib / Publish wrapper](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124869357056) | Linux 10.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Build sdist](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868568272) | Linux 9.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Build wheels (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868568282) | macOS 8.9 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Build wheels (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868568291) | ARM 11.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Build wheels (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868568340) | Windows 13.8 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Build wheels (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868568301) | Linux 10.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Parse version](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518440) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Py FFI / Publish to PyPI](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124876700281) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Swift Lib / Build wrapper package](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868570063) | macOS 0.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Swift Lib / Check FFI publisher](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518447) | Linux 0.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Swift Lib / Publish dry-run](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879095921) | Linux 0.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Release Swift Lib / Publish to Swift Package mirror](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879962652) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [Release Swift Lib / Verify staged package resolves](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124879095935) | macOS 0.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [Shipping sizes](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124881866814) | ARM 23.4 | success | Published artifact sizes / summary; Pages owned by 7510 |
| [Tests (gate)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517727) | ARM 37.6 | failure | Full diff-independent local gate; relay stack failure; alert |
| [Tests (rs audio-devices)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517634) | ARM 3.7 | success | Null-sink audio device contract absent on plain runner; alert |
| [Tests (rs cli-export)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517722) | ARM 5.7 | success | Paced CLI process export contract; alert |
| [Tests (rs doctest --workspace)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517732) | ARM 8.7 | success | Rustdoc examples are not nextest tests; alert |
| [Tests (rs multicast)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517686) | ARM 6.1 | success | Looped-back mDNS discovery contract; alert |
| [Tests (rs quic-sim)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517895) | ARM 10.8 | success | Fresh-seed QUIC workload beyond fixed PR seeds; alert |
| [Tests (rs quic-stress)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517588) | ARM 3.8 | success | QUIC backend pressure and flow-control contract; alert |
| [Tests (rs render-sw)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517583) | ARM 9.2 | success | Software-Vulkan renderer readback; alert |
| [Tests (rs send-depth)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517735) | ARM 2.6 | success | Pinned-nightly Send proof at normal recursion depth; alert |
| [Tests (rs uring)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517772) | ARM 11.7 | success | Different feature/kernel/socket contract; alert |
| [Tests (rs v4l2)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517784) | ARM 3.2 | success | Vivid virtual-camera capture and device lifetime; alert |
| [Tests (test drain)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517760) | ARM 8.0 | success | Two-relay JS viewer drain across the wire; alert |
| [Tests (test drill impaired)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517608) | ARM 5.0 | success | Seeded bufferbloat and packet overflow beyond PR loopback; alert |
| [Tests (test drill-sensitivity)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517783) | ARM 14.2 | success | Drill negative controls prove acceptance catches faults; alert |
| [Tests (test json-properties)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517788) | ARM 6.4 | success | Extended generated JSON protocol cases; alert |
| [Tests (test qmux)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517907) | ARM 6.3 | success | Cross-language per-draft qmux interoperability; alert |
| [Tests (test resume-model)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517809) | ARM 7.1 | failure | Fresh generated resume/range cases; missing-group counterexample; alert |
| [android / Android](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517795) | Linux 3.4 | success | Distinct platform/consumer build or runtime boundary / failure alert |
| [audio-replay / Replay](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518303) | ARM 4.5 | success | Deterministic trace-replay budgets / failure alert |
| [c / C (clang)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518038) | Linux 4.5 | success | Distinct platform/consumer build or runtime boundary / failure alert |
| [cpp / C++ (clang)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518343) | Linux 6.3 | success | Distinct platform/consumer build or runtime boundary / failure alert |
| [features / Features (rs features)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518151) | ARM 53.1 | failure | Feature isolation beyond default gate / relay stack alert |
| [ietf-conformance / moxygen](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517493) | Linux 29.8 | success | Third-party protocol conformance / failure alert |
| [moq-c / Build (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518055) | macOS 23.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-c / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517939) | ARM 10.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-c / Build (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518048) | Windows 14.9 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-c / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518066) | Linux 12.2 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-c / Release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124874459428) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [moq-cli / release / Attest](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124878066994) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [moq-cli / release / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518682) | ARM 11.5 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-cli / release / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518684) | Linux 12.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-cli / release / Build Windows zip (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518562) | Windows 21.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-cli / release / Build macOS tarball (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518608) | macOS 38.1 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-cli / release / Release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124878067934) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [moq-gst / Build tarball (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517858) | macOS 21.7 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-gst / Build tarball (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517671) | ARM 7.4 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-gst / Build tarball (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517835) | Linux 9.8 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-gst / Package .deb/.rpm (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517712) | ARM 4.9 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-gst / Package .deb/.rpm (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517785) | Linux 5.9 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-gst / Release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124873960333) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [moq-relay / release / Attest](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124872230170) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [moq-relay / release / Build (aarch64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518621) | ARM 6.6 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-relay / release / Build (x86_64-unknown-linux-gnu)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518604) | Linux 7.5 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-relay / release / Build Windows zip (x86_64-pc-windows-msvc)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518642) | Windows 12.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-relay / release / Build macOS tarball (aarch64-apple-darwin)](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518513) | macOS 15.0 | success | Keep target/package contract above / failure alert; artifact is build input or diagnosis |
| [moq-relay / release / Release](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124872230844) | 0.0 | skipped | Tag-only publication or native reuse; not executed |
| [oss-scanner / Build](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518381) | ARM 9.2 | success | Distinct platform/consumer build or runtime boundary / failure alert |
| [platform / Platform scope](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518486) | ARM 0.2 | success | Platform selection |
| [platform / Windows](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868585460) | Windows 18.2 | failure | Native OS-specific tests / alert; relay stack failure |
| [platform / macOS](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868585440) | macOS 57.5 | success | Native OS-specific tests / alert |
| [runner-vm / check](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517755) | Linux 7.1 | success | Runner image/boot contract / failure alert; infrastructure-owned |
| [x11 / SHM and GetImage](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518062) | ARM 6.2 | success | Distinct platform/consumer build or runtime boundary / failure alert |

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
