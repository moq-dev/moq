# [M] CI pipeline audit

## Goal

Every merge queue and nightly job is timed, and every expensive step either
earns its minutes or is cut, moved or planned. The Rust cache reuses
unchanged crates, and CI compiles nothing it doesn't need. Fixes land in this
quest's PR, one commit each. Ranking individual tests stays with the
recurring [test cost audit](/quest/m1/audit-test-cost.md).

## Plan

Decided 2026-10-19: a one-time m0 audit, separate from the recurring test cost
audit. It also owns the cache fixes. Storage moved to R2, written by the
merge queue and `main`; [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md)
measures it.

Measured 2026-10-19 (ubuntu-24.04-arm; sample of 20 PR and 20 queue runs of
check.yml, plus 35 wider runs). Start from these levers and re-measure each
before and after. Before one-pass, queue run
[39471110155](https://github.com/moq-dev/moq/actions/runs/39471110155)
(2026-10-20) took 21m08s overall: Check 20m57s and Test 19m36s. Record
per-entry wall time after the one-pass workflow first runs in the queue; draft
PRs cannot supply that measurement.

- **Test's extra builds:** `play` (`-p moq-cli --features play,openh264`)
  compiles for 104 s on a hit and 170 s on a miss to run 2 s of tests; the
  capture build compiles for 38 to 56 s to run 9 s. Test is the long pole.
  Fold them into the main nextest build's feature set, or move them to
  nightly.
- **Cache key:** it hashes all of `flake.nix` and `flake.lock`, so any input
  bump rolls it, including quest bumps unrelated to Rust: 13 rolls in 7 days.
  41% of Rust jobs ran cold. Key on what restored artifacts actually depend
  on: the toolchain's and the native libraries' store paths. Build scripts
  link against those paths, which is why the key covers the flake today.
  The R2 cache has no key of ours: mbx keys each compilation itself.
- **Cold warm timeout:** a cold `cache.yml` warm takes 58 to 60 minutes
  against `timeout-minutes: 60`. On 10-19, two of three were cancelled while
  saving, so cold windows lasted up to 3.5 hours. The R2 cache drops the
  warm: the merge queue publishes what it builds.
- **Workspace crates rebuild on a hit:** 76 workspace units recompile in Test
  and 112 in Check even when the cache hits, and mbx's object store reports 0
  hits in 914 of 960 sessions. The likely cause is that checkout resets file
  mtimes, so cargo's fingerprints see every workspace file as newer. Verify
  that, then use checksum freshness, or backdate only files identical to the
  commit the cache was built from (`git diff --name-only <cache-sha>` keeps
  its fresh mtime). Never backdate a changed file: Cargo would then treat
  the crate as fresh and the queue would pass on the old build. A cache hit
  currently saves only about 2.5 minutes net of its 2.5-minute restore.
- **Release-profile builds in the gate:** dart's moq-ffi release build (fat
  LTO) and the `wasm-release` build. The one-pass workflow
  moves the permutations; confirm nothing release-profile stays in the
  queue.
- **Dev shell:** `nix develop` downloads for 52 s (p90 90 s) in every job,
  even ones that compile no Rust. That overlaps with
  [the Nix cache](/quest/m0/ci/nix-cache-verify.md); measure what is still
  slow once it lands.
- **Waste in the dependency graph:** both rustls backends (ring from moq-quic's
  default `rustls-ring`, aws-lc-rs from rustls, str0m, jsonwebtoken, rcgen and
  object_store), and aws-lc-sys recompiling 3 to 5 times per cold job because
  differing env or features change its hash. bindgen and clang-sys come
  through moq-vaapi, plus gstreamer-sys, openh264-sys2 and tikv-jemalloc-sys.
  52 crate names are duplicated on aarch64-linux (syn 1, 2 and 3; rand 0.8,
  0.9 and 0.10; three toml versions; sha2 and digest 0.10 and 0.11). Cut what
  the gate doesn't need. Dependency upgrades belong to
  [the dependency audit](/quest/m1/audit-deps.md).
- **Nightly:** about 1250 runner-minutes and 2 hours of wall time, and the
  last 5 runs failed (the Features job, plus JS benchmarks cancelled at 60
  minutes). With main's optional checks moved to nightly, a red nightly hides
  breaks, so get it green and keep each job worth its minutes.
- Add cargo `--timings` output, or an equivalent, to the CI jobs so per-crate
  costs stay visible after this audit.

Scratch data from the 2026-10-19 measurement (job logs and scripts) was
local only; re-measure on the one-pass pipeline rather than trusting those
numbers.

## Related

- [Faster runners](/quest/m0/ci/runners.md) - waits on this audit's result
- [CI workflow audit](/quest/m1/audit-ci.md) - the recurring per-workflow audit that starts from this one's result
