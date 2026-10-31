# [M] Compile time audit

## Goal

The crates, dependencies, and code that dominate build time are known, with
recorded baselines, and the worst are fixed or planned.

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

Scope: this audit owns per-crate build cost in one build (dev, release, and
incremental). The [CI workflow audit](/quest/m2/audit-ci.md) owns job-level CI
cost and caching; the [dependency audit](/quest/m2/audit-deps.md) owns
deduplicating and upgrading dependencies.

What to check first:

- [moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176) (moq-net
  compiles once) landed after the m1 measurements, so rerun
  `cargo llvm-lines` on the moq-tokio lib and compare the transport rows
  below, then rerun the thin-LTO comparison against the smaller
  `libmoq_ffi.a`. On `d55407256` (m0, method in git history) thin grew the Go
  mirror's x86_64 archive from 71.23 to 81.07 MiB under its 100 MiB per-file
  limit, cut the final `moq` and `moq_ffi` units' wall by 26 to 41% at two
  jobs with CPU time even, and cost 1.6 to 2.1% more instructions in the
  moq-net session benches. Maintainer decision 2026-10-20: keep fat LTO
  until the archive shrinks, and revisit sooner if release builds join the
  PR gate.
- The last per-reader `Any::decode_atom` copy in moq-mux: `fmp4/text.rs`
  decodes from a bare `&[u8]` (13,307 lines, 1.3% of the lib) while every
  other fMP4 path uses `Cursor<&[u8]>`. It was skipped for
  [moq-dev/moq#6379](https://github.com/moq-dev/moq/pull/6379), which has
  since merged, so it is ready to fix.
- Measurement pitfalls found by [moq-dev/moq#7146](https://github.com/moq-dev/moq/pull/7146)
  (added 2026-10-28): run `perf stat` with `--no-scale`, since without it the
  two core types' counters are each scaled to the whole run and overcount up to
  2x on the hybrid host. And measure an edit through mbx, whose
  `learned_incremental` compiles the edited crate incrementally; plain cargo
  under the host's `incremental = false` rebuilds it whole. Measured with
  incremental on, a one-line moq-net edit costs about 7.5 G instructions to
  check, against 26.3 G whole.
- Whether `cargo-llvm-lines` joined the pinned shell. `flake.nix` had four
  open PRs at the m1 run, so it was not added.

### Baselines (m1 run, 2026-10-23)

Pinned Nix shell (Rust 1.98.1), Linux x86_64, 32 logical CPUs, Cargo bounded
to two jobs, `MBX_DISABLE=1`, one fresh target dir, registry present. The
host's `~/.cargo/config.toml` sets `incremental = false`, so every touched
crate rebuilds whole. The host was shared; load is the one-minute average
sampled every 30 s. Times are single observations: compare shares within a
run, never seconds across runs. Only the llvm-lines counts are deterministic.

Builds on `0d3b3e084` (release with the moq-mux fix below):

| Build | Load (median) | Wall | Unit time | Longest unit |
| --- | --- | ---: | ---: | --- |
| `cargo build --locked --timings` (default members, clean) | 4.1 to 9.0 (5.6) | 308 s | 614 s | aws-lc-sys build script 114 s, 18.6% |
| `cargo build --locked --release -p moq-cli --timings` (clean deps) | 3.1 to 17.7 (5.4) | 750 s | 1,127 s | `moq` bin 369 s, 49% of wall |
| Default members after a comment appended to `rs/moq-net/src/lib.rs` | 5.0 to 8.4 | 70 s | 126 s | moq-tokio 21.8 s, 17.4% |

Clean dev: build scripts are 31% of unit time (aws-lc-sys alone 18.6%, ring
1.7%) and workspace crates 18.6%; the two-job queue stays full, so total
unit time sets the wall, not the critical path. Release: the final `moq`
unit is still fat LTO on one codegen unit (m0: 42 to 53% of the wall), and
aws-lc-sys is 11% of unit time. Incremental: a moq-net comment rebuilds
every dependent; moq-tokio (17.4%), the `moq` bin (15.6%), moq-net (10.9%),
moq-mux (9.3%) and moq-relay (8.6%) lead.

`cargo llvm-lines` 0.4.48 (`nix shell nixpkgs#cargo-llvm-lines`), dev profile,
default features:

| Target | LLVM IR lines | Copies | m0 lines | Notable share |
| --- | ---: | ---: | ---: | --- |
| moq-tokio lib | 1,679,222 | 48,553 | 1,661,503 | moq-net rows instantiated over a transport session 39.1% (any row naming one 55.7%); usage-rs derive 5.2% |
| moq-mux lib | 996,345 | 28,525 | 908,532 | serde 18.6%; mp4-atom 11.9% |
| moq-cli `moq` bin | 1,108,118 | 39,327 | 1,037,734 | tokio 16.6%; serde 5.2%; usage-rs derive 2.7% |
| moq-ffi lib | 811,228 | 32,309 | not run | tokio 18.6%; uniffi_core 9.6%; moq-mux generics 7.3% |
| moq-net lib | 799,404 | 25,306 | 686,443 | BTreeMap internals 29.5%; rows naming `moq_net::model` 47.7% |
| moq-relay lib | 667,136 | 24,636 | 711,268 | serde 30.4% (toml 9.3%); usage-rs derive 9.2% |
| moq-relay bin | 311,790 | 11,313 | 384,813 | tokio 18.9%; transport-session rows 3.0% |
| qmux lib | 12,528 | 524 | not run | generic, instantiated downstream |

Transport rows: in moq-tokio, `qmux::session::Session` names 28.6% and
`moq_tokio::transport::Session<quic::web::Session>` 28.2%, each again under
moq-net's IETF `ControlStreamAdapter` (20.0%), so the IETF half compiles four
times. The m0 share (43.2%) used an unrecorded grouping; compare against the
39.1% definition above (first path segment `moq_net`, name contains
`qmux::session::Session` or `moq_tokio::transport::Session`).

moq-net grew 16.5% since m0, mostly in `model`; its BTreeMap share rose from
24.1% to 29.5% across about 30 distinct key and value types, each
instantiating the whole map.

### Results of the m1 run

- Fixed: moq-mux's fragment decode read through `Cursor<&Bytes>`, a fourth
  instantiation of mp4-atom's `Any::decode_atom`; it now uses
  `Cursor<&[u8]>` like the rest (moq-mux lib 1,006,004 to 996,345 lines).
- Deferred: [moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176)
  landed after these measurements, so the transport-share and thin-LTO
  comparisons move to the m2 run.
- Explained, no action: aws-lc-sys's build script remains the longest unit
  in clean builds; the crypto provider stays (m0). usage-rs derive output
  scales with config fields and is external code. serde visitors per
  deserializer are inherent to TOML config. moq-rtmp, moq-srt and moq-rtc
  entry points generic over `impl Into<Path>` instantiate their async bodies in
  the `moq` bin (20,327 lines with drop glue, 1.8%); a non-generic inner
  function would move them, but adds code for little. The catalog JSON diff instantiated
  per crate is [catalog publish once](/quest/m2/catalog-publish-once.md).
- Not run this time: `-Z time-passes` on the `moq` unit (m0: fat LTO 74%,
  LLVM passes 23%); the release share did not move enough to warrant it.

### Method

Per target, `CARGO_TARGET_DIR=.tmp/target CARGO_BUILD_JOBS=2
MBX_DISABLE=1 cargo llvm-lines --locked -p <crate> --lib` (or `--bin`; name
`qmux@<version>` since a dev-dependency pins an older qmux), then group rows
by the first path segment after stripping crate hashes, and by substrings for
the transport session types. Clean dev and release builds use `--timings`
into the same fresh target dir; the incremental probe appends one comment
line to `rs/moq-net/src/lib.rs` after a no-op catch-up build. Sample
`/proc/loadavg` every 30 s during each run.

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
