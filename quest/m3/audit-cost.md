# [L] Cost audit

## Goal

Every CI workflow and job, test, dependency, second of build time, and byte
of shipped artifact earns its cost. What doesn't is deleted by default, or
else made cheaper or moved, and growth since the previous run is explained,
fixed, or planned. Runner and cache infrastructure belongs to
[CI cost and latency](/quest/m0/ci/README.md).

## Plan

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, with its own `Required`
naming the [release cut](/quest/m3/release/cut.md) and Check first rewritten. Fixes land
in that PR; only maintainer decisions (public API, scope) and changes too
large to review alongside become follow-ups.

Default to removing (decided 2026-11-01, maintainer): the PR body lists each
deletion with its measured cost and why it wasn't worth it. Removing a public
API, a user-facing feature, or a release target stays a follow-up.

Previous runs: CI [moq-dev/moq#7603](https://github.com/moq-dev/moq/pull/7603),
tests [moq-dev/moq#7600](https://github.com/moq-dev/moq/pull/7600), build time
[moq-dev/moq#7602](https://github.com/moq-dev/moq/pull/7602) (raw data in
[compile-time-audit.json](/.github/compile-time-audit.json)), size
[moq-dev/moq#6541](https://github.com/moq-dev/moq/pull/6541), dependencies
[moq-dev/moq#7601](https://github.com/moq-dev/moq/pull/7601).

### Check first: CI

- Table every workflow in `.github/workflows/` and every job of the latest
  scheduled Nightly by signal (what it catches that nothing cheaper does),
  visibility (a gate, a Discord `#alerts` post through
  [alert.yml](/.github/workflows/alert.yml), a Pages report, or a job summary;
  a result only in an artifact or log is invisible), and cost (runner-minutes
  by runner type; macOS is the scarce one).
- The [benchmark history](https://git.moq.dev/bench/)'s `bench-gate-cost`
  `module/*` rows give each `just ci` module's wall and CPU seconds.
- Sum elapsed runner-minutes from the REST jobs API. Org billing is monthly
  attribution only; never derive a workflow's cost from it.
- Read failures by cause, not job count: N fuzz targets failing one build is
  one finding, and a green compile is not a caught regression. Count
  infrastructure flakes across nights before acting.
- Each Pages report owns one `gh-pages` directory and joins the
  `benchmark-pages` concurrency group (decided 2026-10-20;
  `sh/gh/pages-publish.sh`).
- Still the maintainer's call since m0: fold the capture and play test builds
  into the main test build, or move both to Nightly. Keep both until decided.
- Shared default-feature FFI builds feed Go, Kotlin, Swift, C, and C++; Dart
  has its own target set. Matching target names are not redundant outputs.

### Check first: tests

- The benchmark history's `cold/rust-test/*` (each test binary's summed slot
  time) and `cold/js-test/*` (each Bun test file's) rows.
- Measure nextest's per-test elapsed time as slot time from one invocation,
  noting its test count, profile, features, and threads. Repeat under
  `quest gate` and pin whole-suite comparisons to the same CPUs; a
  three-second swing can be QUIC draining.
- Classify a failure before calling it a flake. Flake quests own their tests;
  don't delete them to green the audit.
- Kept on purpose: the compressed-cache bound tests (stored-tail shedding and
  bounded inflation), the three io_uring endpoint timeout and backlog tests,
  macro UI compile failures, GStreamer reconnects, the CLI export lane's
  redundant-importer arms (real process failover), and the CLI report's
  mid-import, late-sink, and stdin-EOF tests. Each covers a distinct boundary.

### Check first: build time

- The benchmark history's `crate/*/instructions` rows rank every crate's
  rustc instructions in a cold gate and after a moq-net edit.
- Fat LTO stays (maintainer, 2026-10-20; reaffirmed 2026-11-02): thin cut the
  shipping build's wall about 12% and grew `libmoq_ffi.a` about 15%, but
  release builds are off the PR gate while a runtime cost would reach every
  user. Compare the two only from fresh targets.
- Rejected, don't repeat: sharing moq-net's BTree key types
  ([experiment](/.github/btree-build-audit.md)); moving MP4 readers from
  bare slices to cursors, since mp4-atom's own decoders introduce both forms;
  generic transport session bodies, which
  [moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176) removed.
- Measure edits through mbx and repeat each: the first edit records
  incremental state. In a warm edit the final binaries are about half the
  unit time. On the hybrid host use `perf stat --no-scale`.
- Rows built with dev-dependencies shift across the cargo-hakari
  workspace-hack ([moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985)),
  which excludes native media crates from unification
  ([moq-dev/moq#7713](https://github.com/moq-dev/moq/pull/7713)).

### Check first: artifact size

- Compare the first complete Shipping sizes report after the cut with the
  previous run's, the [2026-10-31 measurements](https://github.com/moq-dev/moq/blob/cd560bde5e585dcfc59a705b8013a9a733c06a73/quest/m3/audit-size.md). Never compare an archive to an ELF, different targets, or
  symbol-preserving to stripped builds. JS rows are synthetic first-load
  bundles from `sh/size/bundles.ts`; attribute only an entry's static import
  closure and normalize hashed filenames.
- `nm -S -C --defined-only` on the shipped static libraries attributes native
  growth by namespace.
- Kept: the whole Rust workspace in the Python sdist, minus `test_data`
  ([moq-dev/moq#6867](https://github.com/moq-dev/moq/pull/6867)).
  `opt-level = "s"` for the bindings needs codec, crypto, and FFI throughput
  measurements first.

### Check first: dependencies

- Research each candidate's maintenance, performance, and API fit before a
  swap. Manual bumps respect Dependabot's 7-day cooldown; security fixes
  bypass it.
- What Dependabot misses: majors and 0.x minors (`cargo upgrade --dry-run
  --incompatible allow --pinned allow --ignore-rust-version`, `bun outdated
  -r`, and `bun outdated` in each standalone Bun project), `0.0.x` ranges and
  `npm:` aliases, and lockfiles missing from `.github/dependabot.yml`.
  `dart/moq` and `nix/uniffi-dart-Cargo.lock` are uncovered on purpose.
- The QUIC fork has no advisory feed: list quinn-proto and quinn-udp
  advisories on RustSec and GHSA, port any fix, and note it in
  `rs/moq-quic/README.md` (decided 2026-10-28). Only a
  [vendor sync](/quest/m3/vendor-sync/quinn.md), which reviews the full
  upstream range, advances the recorded upstream commit.
- Waiting on upstream: [ashpd](/quest/m4/ashpd-maintained.md) and
  [askama](/quest/m4/askama-maintained.md); the root Bun overrides for glob,
  minimatch, chokidar, postcss-selector-parser, and Vite, each removable once
  its parent allows a safe range; `deny.toml`'s proc-macro-error2 ignore,
  until core-models moves to hax-lib 0.4; paste through tikv-jemalloc-ctl;
  http-cache-reqwest's 1.0 prerelease; and the local `TransportStats` in
  `js/net/src/connection/stats.ts`, until TypeScript's DOM has
  `WebTransportConnectionStats`.
- Most duplicate versions in `Cargo.lock` come from upstream ecosystem
  transitions; check the consumer before calling one removable. Fix a
  transitive Bun advisory with a targeted `bun update <pkg> --lockfile-only
  --ignore-scripts`, not a regenerated lockfile.
- Kept on purpose, each with live consumers a replacement would reimplement:
  `tracing-android`, `humantime-serde`, `hex`, `send_wrapper`, `byteorder`,
  `assert_matches`, `cpu-time`, `stats_alloc`, `unsafe-libopus`, the
  QUIC/UDP, RTMP, V4L2, NVIDIA, VA-API, UniFFI, and LibAV forks
  ([vendor-sync](/quest/m3/vendor-sync/README.md) ports upstream), and JS's
  Zod, pako, and UUIDv7. Replaced: `sorted-btree`, by the ledger's in-tree
  chunked `Sorted` list (about 5 KB Brotli per bundle). `moq-gst` keeps its Rust 1.92 floor
  for GStreamer.

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
