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

### Tools

Nightly keeps both reports as artifacts: `just rs coverage` (cargo-llvm-cov
over nextest, the native workspace at default features, both fuzz packages
and the native moq-wasm stub excluded) and `just js coverage` (Bun's own
coverage, one process per package, line hits unioned across processes).
Start from those artifacts rather than re-running the instrumented
workspace, which is expensive on a shared machine.

- `just rs coverage <crate>` is not comparable to the workspace table:
  without other packages' feature unification it compiles less of the
  crate. moq-video measured 4553 / 7741 lines alone against 6431 / 9965 in
  the workspace run.
- Default features hide feature-gated tests that do run nightly in the
  `rs features` leg (`--all-features`), such as moq-video's encode producer
  (`capture`, `openh264`). Check that leg before calling a low file untested.
- Bun 1.4.2 undercounts a module loaded again with a query suffix (signals
  reported 19.38% in a run where it was 89.80%). Verify an apparent JS gap
  before acting on it, and never merge packages into one `bun test`
  process: package-level mocks then contaminate unrelated tests.

### m0 baselines

From `origin/main` at `2f1f062c6`: 8,848 native tests passed with 35 ignored,
and 18 JS package processes ran 3,678 tests. Lines covered / measured:

| Rust crate | Lines | Rust crate | Lines |
| --- | --- | --- | --- |
| hang | 2848 / 2924 | moq-loc | 141 / 153 |
| kio | 2023 / 2115 | moq-msf | 674 / 684 |
| moq-archive | 4351 / 4480 | moq-mux | 63457 / 65700 |
| moq-audio | 5346 / 5610 | moq-net | 80294 / 84169 |
| moq-auth | 4894 / 5006 | moq-nvenc | 3445 / 7771 |
| moq-bench | 1614 / 2068 | moq-pattern | 931 / 960 |
| moq-boy | 138 / 671 | moq-quic | 21325 / 22420 |
| moq-c | 8967 / 9562 | moq-relay | 10839 / 11492 |
| moq-cli | 6578 / 7838 | moq-room | 342 / 369 |
| moq-clock | 1571 / 1625 | moq-rtc | 2500 / 2876 |
| moq-clock-macros | 27 / 33 | moq-rtmp | 6816 / 7630 |
| moq-e2ee | 679 / 759 | moq-rtsp | 1033 / 1057 |
| moq-ffi | 10756 / 11705 | moq-shaper | 2042 / 2116 |
| moq-flate | 839 / 892 | moq-sock | 1316 / 1486 |
| moq-gst | 6320 / 6698 | moq-srt | 1286 / 1481 |
| moq-hls | 8360 / 8741 | moq-stats | 4413 / 4600 |
| moq-ice | 319 / 361 | moq-tokio | 15702 / 17740 |
| moq-json | 4903 / 5237 | moq-transcode | 2758 / 2987 |
| moq-uring | 4643 / 5391 | moq-v4l | 9 / 1457 |
| moq-video | 6431 / 9965 | | |

| JS package | Lines | Unloaded files | JS package | Lines | Unloaded files |
| --- | --- | --- | --- | --- | --- |
| common | 200 / 250 | 5 | loc | 127 / 145 | 0 |
| net | 17812 / 23595 | 4 | msf | 103 / 130 | 0 |
| p2p | 293 / 303 | 1 | video | 277 / 336 | 0 |
| clock | 286 / 300 | 1 | watch | 3075 / 4547 | 31 |
| pattern | 435 / 523 | 0 | publish | 2773 / 3308 | 22 |
| auth | 720 / 783 | 2 | room | 493 / 530 | 1 |
| e2ee | 568 / 569 | 2 | signals | 565 / 747 | 3 |
| flate | 233 / 333 | 0 | wasm | 0 / 0 | 0 |
| json | 831 / 1141 | 0 | moq-boy | 155 / 194 | 8 |
| hang | 2044 / 2859 | 1 | | | |

These are not acceptance thresholds. Compare the next run's report against
them and explain what moved.

### What m0 found

Two shipped bugs hid in code no test executed: echo cancellation never
received its playback reference on a 44.1 kHz output (the resampled tap was
never marked read), and two DOM helpers dropped children or registered
cleanup against an emptied fragment. Both were found by running code that
existed but had no caller, which makes ignored and unloaded code the first
place to look.

- Every `#[ignore]` carries a reason. Ignored benchmarks run in
  `sh/gh/nightly.sh rust-test`; the m0 run gave a nightly recipe to every
  other one a hosted runner can host: `rs quic-stress`, `rs audio-devices`
  (a private PulseAudio null sink), `rs multicast` (mDNS loops back in the
  kernel), and `rs render-sw` (wgpu on Mesa lavapipe). An `#[ignore]` whose
  caller is neither a recipe nor a quest is the first finding.
- Hardware-only tests map to [GPU CI](/quest/m2/gpu-ci.md) (NVIDIA, and the
  host Vulkan driver the VA-API renderer import tests also need inside the
  Nix shell), [Video hardware validation](/quest/m3/video-hardware.md)
  (Windows, Metal, cameras, portals), [Windows.Graphics.Capture](/quest/m2/capture-wgc.md),
  [MediaCodec on an emulator](/quest/m2/android-mediacodec-ci.md), and
  [V4L2 virtual camera](/quest/m2/v4l2-virtual-camera.md). Check whether
  those landed before re-auditing the same tests.
- Interop: [Bindings in the interop matrix](/quest/m2/interop-bindings.md)
  adds Kotlin, Swift, and Dart; until it lands, those wrappers' network
  delivery is untested.
- JS: `<moq-watch-ui>` runs in the interop player, and `<moq-publish-ui>`
  has a Chromium test in `just test harness`. The React signals adapter is
  still untested; it needs a DOM renderer. The remaining unloaded files are
  type-only, worklets covered by the browser lanes, or formatters.
- The last 25 `fix` commits before the m0 run all carried a regression test
  or were compile/link checks. Repeat that sample from the m0 PR forward.
