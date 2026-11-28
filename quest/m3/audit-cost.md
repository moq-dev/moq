# [L] Audit build and CI cost

## Goal

Run the `cost` audit: `quest audit cost` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-23 at `890ea4e531` (Nightly `45242597151` at `b29b90be29`: 219 jobs, **3,054.0 runner-minutes** total: `ubuntu-24.04-arm` 2,129.0m, `macos-latest` 291.5m, `ubuntu-24.04` 291.4m, `ubuntu-latest` 143.9m, Windows 135.5m, `self-hosted` 11.9m). The 12 `mutants` shards accounted for 847.7m (28% of Nightly). Shipping sizes baseline at `b29b90be29`: `js/net` 158.7 KiB brotli, `js/hang` 180.4 KiB, `js/watch` 203.2 KiB, `js/watch-ui` 214.1 KiB, `js/publish` 208.3 KiB; `moq-relay` Linux x64 11.6 MiB, `moq-cli` Linux x64 20.7 MiB; `moq-ffi` `.text` 16.5 MiB (`moq_net` 4.2 MiB / 25.6%, `std` 3.6 MiB / 22.2%).

Previous runs: CI [moq-dev/moq#7603](https://github.com/moq-dev/moq/pull/7603), tests [moq-dev/moq#7600](https://github.com/moq-dev/moq/pull/7600), build time [moq-dev/moq#7602](https://github.com/moq-dev/moq/pull/7602) ([compile-time-audit.json](/.github/compile-time-audit.json)), size [moq-dev/moq#6541](https://github.com/moq-dev/moq/pull/6541). Runner and cache infrastructure belongs to [CI cost and latency](/quest/m0/ci/README.md); dependencies belong to [audit-deps](/quest/m3/audit-deps.md).

### Check first

- CI: table `.github/workflows/` and latest Nightly jobs by signal, visibility (`alert.yml`, Pages, job summary), and runner-minutes (macOS is the scarce runner). Watch Nightly `mutants` cost (848m) and `moq-cli` macOS build time (35.8m). Each Pages report owns one `gh-pages` directory under `benchmark-pages` (`sh/gh/pages-publish.sh`). Shared default-feature FFI builds feed Go/Kotlin/Swift/C/C++ while Dart has its own targets (not redundant). Still maintainer's call since m0: fold capture/play test builds into main test build or move to Nightly.
- Tests: inspect `cold/rust-test/*` and `cold/js-test/*` history; measure nextest slot time under `quest gate` pinned to the same CPUs. Kept on purpose: compressed-cache bound tests, io_uring timeout/backlog tests, macro UI compile failures, GStreamer reconnects, CLI export redundant-importer arms, and CLI report mid-import/late-sink/stdin-EOF tests.
- Build time: inspect `crate/*/instructions` cold and incremental rows. Fat LTO stays (reaffirmed 2026-11-02). Rejected, do not repeat: sharing moq-net BTree key types ([experiment](/.github/btree-build-audit.md)), moving MP4 readers from bare slices to cursors, generic transport session bodies ([moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176)). Measure edits through `mbx` after warming incremental state, using `perf stat --no-scale` on hybrid hosts.
- Artifact size: compare complete Shipping sizes reports like-for-like; attribute native growth with `nm -S -C --defined-only` and JS bundles via `sh/size/bundles.ts`. Kept: whole Rust workspace in Python sdist minus `test_data` ([moq-dev/moq#6867](https://github.com/moq-dev/moq/pull/6867)). `opt-level = "s"` for bindings needs codec/crypto/FFI throughput measurements first.

## Audit

- cost

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
