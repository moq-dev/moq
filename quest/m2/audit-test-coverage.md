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
excluded) and `just js coverage` (Bun's own
coverage, one process per package, line hits unioned across processes).
Start from those artifacts (`gh run download <nightly run> -n rust-coverage`
and `-n js-coverage`) rather than re-running the instrumented workspace,
which is expensive on a shared machine.

- `just rs coverage <crate>` is not comparable to the workspace table:
  without other packages' feature unification it compiles less of the
  crate.
- Default features hide feature-gated tests that run nightly in the
  `rs features` leg (`--all-features`). Check that leg passed before calling
  a low file untested: on 2026-10-20 it stopped at a moq-auth example's lint,
  so nothing after it ran.
- Bun 1.4.2 undercounts a module loaded again with a query suffix. Verify an
  apparent JS gap before acting on it, and never merge packages into one
  `bun test` process.
- The most productive scan in m1 was public functions whose signature line
  the lcov report never hit: walk `DA:` records for lines matching
  `pub fn`/`pub(crate) fn` outside test, bench, and fuzz files. It found the
  binding facades below in minutes. Per-file uncovered line counts mostly
  point at GPU code and generated bindings instead.
- When sampling `fix` commits for regression tests, count
  `#[moq_clock::test]` as well as `#[test]` and `#[tokio::test]`.

### m1 baselines

From nightly run 39421743227 at `f13283b302` (2026-10-20): 9,050 native
tests passed with 33 ignored, and 18 JS package processes ran 3,924 tests.
Lines covered / measured:

| Rust crate | Lines | Rust crate | Lines |
| --- | --- | --- | --- |
| hang | 2848 / 2924 | moq-loc | 141 / 153 |
| kio | 2045 / 2137 | moq-msf | 674 / 684 |
| moq-archive | 4353 / 4484 | moq-mux | 64372 / 66631 |
| moq-audio | 5455 / 5716 | moq-net | 83698 / 87593 |
| moq-auth | 5187 / 5313 | moq-nvenc | 3445 / 7771 |
| moq-bench | 1614 / 2068 | moq-pattern | 931 / 960 |
| moq-boy | 169 / 707 | moq-quic | 21696 / 22815 |
| moq-c | 8946 / 9516 | moq-relay | 10952 / 11607 |
| moq-cli | 6431 / 7566 | moq-room | 432 / 460 |
| moq-clock | 1858 / 1921 | moq-rtc | 2506 / 2882 |
| moq-e2ee | 679 / 759 | moq-rtmp | 6826 / 7638 |
| moq-ffi | 10819 / 11749 | moq-rtsp | 1033 / 1057 |
| moq-flate | 839 / 892 | moq-shaper | 2149 / 2219 |
| moq-gst | 6362 / 6740 | moq-sock | 1328 / 1486 |
| moq-hls | 8473 / 8857 | moq-srt | 1286 / 1481 |
| moq-ice | 319 / 361 | moq-stats | 4414 / 4601 |
| moq-json | 4897 / 5231 | moq-tokio | 15962 / 18005 |
| moq-uring | 4802 / 5558 | moq-transcode | 2997 / 3239 |
| moq-v4l | 9 / 1457 | moq-video | 6462 / 9988 |
| carla bridge (demo) | 923 / 1676 | | |

| JS package | Lines | Unloaded files | JS package | Lines | Unloaded files |
| --- | --- | --- | --- | --- | --- |
| common | 200 / 250 | 5 | loc | 127 / 145 | 0 |
| net | 19248 / 25149 | 4 | msf | 103 / 130 | 0 |
| p2p | 293 / 303 | 1 | video | 277 / 336 | 0 |
| clock | 310 / 333 | 1 | watch | 3123 / 4596 | 29 |
| pattern | 435 / 523 | 0 | publish | 2773 / 3308 | 22 |
| auth | 725 / 788 | 2 | room | 493 / 530 | 1 |
| e2ee | 568 / 569 | 2 | signals | 580 / 763 | 2 |
| flate | 233 / 333 | 0 | | | |
| json | 804 / 1093 | 0 | moq-boy | 155 / 194 | 8 |
| hang | 2076 / 2899 | 1 | | | |

These are not acceptance thresholds. Compare the next run's report against
them and explain what moved. Against m0, every crate held its ratio: the
growth (moq-net +3.4k measured lines, moq-auth's in-band auth, moq-clock)
arrived tested, and moq-cli and @moq/json shrank. The m1 run's new tests
land after this baseline, so the next report should show moq-mux's
`import/track.rs` and moq-ffi's `video.rs` and `media.rs` rise.

### What m1 found

- `just rs audio-devices`, added by the m0 run, failed all five tests on its
  first nightly. Two causes: cpal's PulseAudio host opened playback with the
  server's default 2 s buffer (3.96 s end to end), and the tests assumed a
  device starts within 100 ms while PulseAudio's idle null sink renders up to
  two seconds ahead. Playback now asks PulseAudio for 20 ms periods, and the
  tests wait until the device's clock moves. Watch that the first nightly
  after this run is green, and keep any new device test behind the same wait.
  Capture likely shares the buffer default: [PulseAudio capture periods](/quest/m2/pulseaudio-capture-period.md).
- The binding facades were the largest untested public surface:
  `moq_mux::import::Track` had decoded only Avc3, Hvc1, VP8, and the audio
  codecs other than MP3; `TrackStream` (the FFI raw-stream producer) had
  never decoded a byte; and `MoqVideoEncoder` (`moq.video.Encoder` in every
  wrapper) had no test in any language. Its doc and the Swift copy also
  claimed `finish` drops the trailing access unit, which it does not.
- `just rs gpu` could not load a Vulkan loader inside the Nix shell on a
  non-NixOS host. AMD and Intel now run through the flake's Mesa; NVIDIA
  stays with [GPU CI](/quest/m2/gpu-ci.md).
- The VA-API renderer import tests have no caller: the m0 run mapped them to
  GPU CI, which covers only NVIDIA. [VA-API renderer tests](/quest/m2/vaapi-render-tests.md)
  owns them now.
- Every `#[ignore]` has a reason, and each one's caller is a recipe or a
  quest. Several reasons name neither (the WGC, MediaCodec, and NVIDIA
  Vulkan/CUDA tests), but their quests name the tests. Re-run the check by
  test name, not by reason text.
- All 33 `fix` commits from the m0 PR to `7d2a91fe66` that changed code
  carried a regression test; the rest were dependency, CI, Nix, or bench
  compile fixes.

### What to check first

- The nightly legs that run tests outside `just check`: `rs audio-devices`,
  `rs multicast`, `rs render-sw`, `rs quic-stress`, `rs v4l2`, `rs features`, and the
  `rust-test` benchmark group. A red leg hides everything after it.
- Public functions never executed, by the scan above, starting with moq-c,
  moq-ffi, and moq-mux's import facades.
- Whether [Bindings in the interop matrix](/quest/m2/interop-bindings.md),
  [GPU CI](/quest/m2/gpu-ci.md),
  and [MediaCodec on an emulator](/quest/m2/android-mediacodec-ci.md) landed,
  before re-auditing the tests they own.

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
