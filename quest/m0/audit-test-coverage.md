# [M] Test coverage audit

## Goal

Behavior that matters but no test exercises is covered or planned, and every
test runs in CI.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Starting points (2026-10-19): nightly measures only moq-quic, through
`just rs coverage <crate>` (`cargo llvm-cov`). Extend that recipe to the
remaining crates, pick a tool for `js/`, and record per-crate baselines
here. Look for `#[ignore]` tests and JS tests
that no CI job runs (AGENTS.md wires every test into CI, at least nightly),
fixes merged since the previous run without a regression test, and gaps in the
`just test interop --all` matrix.

### Audit checkpoint (2026-10-19)

Base: `origin/main` at `2f1f062c6`. This run is incomplete; keep this quest
and do not schedule its successor yet.

`just rs coverage` now measures the native workspace through nextest, excluding
both fuzz packages and the empty native moq-wasm stub. The single-crate form
still works. Reports include a JSON summary, LCOV, and HTML. Nightly retains
those artifacts. The instrumented native workspace ran 8,848 tests successfully
with 35 ignored tests; the first report attempt exposed incompatible report
selection flags, which are corrected in the recipe and covered by its tests.

Native default-feature Rust baselines from those 8,848 passing tests:

| Rust crate | Covered / measured lines | Covered / measured regions |
| --- | --- | --- |
| hang | 2848 / 2924 | 4681 / 4857 |
| kio | 2023 / 2115 | 3531 / 3679 |
| moq-archive | 4351 / 4480 | 8934 / 9319 |
| moq-audio | 5346 / 5610 | 8850 / 9482 |
| moq-auth | 4894 / 5006 | 9409 / 9700 |
| moq-bench | 1614 / 2068 | 3081 / 3905 |
| moq-boy | 138 / 671 | 262 / 1223 |
| moq-c | 8967 / 9562 | 16058 / 17814 |
| moq-cli | 6578 / 7838 | 11516 / 14136 |
| moq-clock | 1571 / 1625 | 2911 / 3013 |
| moq-clock-macros | 27 / 33 | 61 / 71 |
| moq-e2ee | 679 / 759 | 1091 / 1255 |
| moq-ffi | 10756 / 11705 | 16773 / 18736 |
| moq-flate | 839 / 892 | 1829 / 1932 |
| moq-gst | 6320 / 6698 | 11901 / 12627 |
| moq-hls | 8360 / 8741 | 16332 / 17002 |
| moq-ice | 319 / 361 | 611 / 675 |
| moq-json | 4903 / 5237 | 9460 / 10116 |
| moq-loc | 141 / 153 | 307 / 333 |
| moq-msf | 674 / 684 | 987 / 1002 |
| moq-mux | 63457 / 65700 | 118478 / 123799 |
| moq-net | 80294 / 84169 | 153425 / 161575 |
| moq-nvenc | 3445 / 7771 | 3942 / 11271 |
| moq-pattern | 931 / 960 | 1830 / 1881 |
| moq-quic | 21325 / 22420 | 33760 / 35389 |
| moq-relay | 10839 / 11492 | 19210 / 20543 |
| moq-room | 342 / 369 | 609 / 662 |
| moq-rtc | 2500 / 2876 | 5050 / 5795 |
| moq-rtmp | 6816 / 7630 | 11832 / 13360 |
| moq-rtsp | 1033 / 1057 | 2372 / 2474 |
| moq-shaper | 2042 / 2116 | 3370 / 3490 |
| moq-sock | 1316 / 1486 | 2082 / 2346 |
| moq-srt | 1286 / 1481 | 2402 / 2743 |
| moq-stats | 4413 / 4600 | 8578 / 8902 |
| moq-tokio | 15702 / 17740 | 26283 / 29820 |
| moq-transcode | 2758 / 2987 | 5037 / 5568 |
| moq-uring | 4643 / 5391 | 7613 / 8855 |
| moq-v4l | 9 / 1457 | 9 / 1743 |
| moq-video | 6431 / 9965 | 11443 / 17891 |

These are cargo-llvm-cov's default source totals, including inline test modules.
They do not measure disabled features, other platforms, or hardware paths that
return early. libmoq, moq-native, moq-clock-macros, and the default-disabled
uniffi-bindgen generator contribute no instrumented source to this report;
moq-wasm and fuzz targets are explicitly outside the native measurement.
The lowest native totals are moq-v4l (0.6% lines), moq-boy (20.6%), moq-nvenc
(44.3%), and moq-video (64.5%). V4L devices, native GUI lifecycle, and GPU
encode/decode are the first paths to validate against existing hardware plans.

`just js coverage` uses [Bun's built-in coverage](https://bun.sh/docs/test/code-coverage)
without adding a tool dependency. Its 18 package/common processes ran 3,678
tests with no failures. Preserve package isolation: a combined `bun test js`
lets package-level mocks contaminate unrelated tests. In particular, publish's
audio encoder tests fail with `broadcast.audio is not a function` in a combined
run, while all 262 publish tests pass in their own process.

The JSON summary unions executed source lines across package processes, since
consumer tests exercise dependencies too. Unloaded files remain listed rather
than disappearing behind an apparently complete percentage. Some are type-only
or re-export modules. These are measured-line baselines, not whole-program
coverage or acceptance thresholds:

| JS package | Covered / measured lines | Unloaded source files |
| --- | --- | --- |
| common | 200 / 250 | 5 |
| net | 17812 / 23595 | 4 |
| p2p | 293 / 303 | 1 |
| clock | 286 / 300 | 1 |
| pattern | 435 / 523 | 0 |
| auth | 720 / 783 | 2 |
| e2ee | 568 / 569 | 2 |
| flate | 233 / 333 | 0 |
| json | 831 / 1141 | 0 |
| hang | 2044 / 2859 | 1 |
| loc | 127 / 145 | 0 |
| msf | 103 / 130 | 0 |
| video | 277 / 336 | 0 |
| watch | 3075 / 4547 | 31 |
| publish | 2773 / 3308 | 22 |
| room | 493 / 530 | 1 |
| signals | 565 / 747 | 3 |
| wasm | 0 / 0 | 0 |
| moq-boy | 155 / 194 | 8 |

Bun 1.4.2 undercounts a source loaded again with a query suffix. With the
cross-package-copy regression enabled, signals/src/index.ts reports 19.38%
line coverage; filtering only that test gives 89.80%. Keep the regression
running, and verify an apparent gap before acting on the counters.

Coverage inspection found two untested DOM-helper bugs: omitted element options
discard children, and array rendering registers cleanup against a fragment
whose children have moved. Each fix carries a Chromium regression in the
existing `just test harness` interop lane. Rendering also cleans up children
passed directly in a DocumentFragment.

Three ignored moq-tokio receive-window stress tests had no CI caller. All three
pass over real sockets (13.1 seconds together), and `just rs quic-stress` now
runs them nightly. The source inventory contains 70 `#[ignore]` attributes;
the compiled native default-feature run exposes 35 ignored tests. GPU, camera,
portal, microphone, and multicast cases need separate environments; do not
broadly enable ignored tests on a hosted runner. Existing hardware work is in
[GPU CI](/quest/m2/gpu-ci.md) and [video hardware validation](/quest/m3/video-hardware.md).

The last 25 `fix` commits were inspected for test changes. Behavioral fixes
carry added or extended regressions; the platform constant and binding-version
fixes are compile/link checks. The current `just test interop --all` matrix has
five publishers (Rust, Python, Go, C++, browser JS) and nine subscribers (those
five, native Node, native Bun, C, GStreamer), with a separate native tail matrix.
It does not cover Kotlin, Swift, or Dart network clients. Binding workflow tests
are useful evidence but do not establish those cross-language network pairs.

Remaining work in this run:

- Finish the behavioral review of the low-covered native entrypoints and
  device paths; source totals alone do not establish lifecycle correctness.
- Finish mapping every ignored test to an actual CI caller or a scoped hardware
  plan. The ignored UNKNOWN-publisher IETF cluster frame test has no dedicated
  quest; it is a real data-path gap, separate from the passing announcement test.
- Review the unmeasured JS paths, especially signals DOM/framework adapters and
  publish/watch UI/worklet code, against browser tests rather than adding tests
  that only mirror implementation.
- Decide and implement any missing interop clients as independently landable
  work if their build/runtime requirements exceed this audit's review scope.

Public API shape and wire impact: none. The DOM changes correct the behavior of
existing helpers; the remaining additions are test and coverage tooling.
