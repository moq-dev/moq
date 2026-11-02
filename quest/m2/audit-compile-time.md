# [M] Compile time audit

## Goal

The crates, dependencies, and code that dominate build time are known, with
recorded baselines, and the worst are removed, refactored to build cheaper, or
planned.

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

Default to removing (decided 2026-11-01, maintainer): whatever doesn't earn its
cost is deleted inline, and the PR body lists each deletion with its measured
cost and why it wasn't worth it. Only what earns its keep is refactored to be
cheaper instead. Removing a public API or a user-facing feature stays a
follow-up.

Scope: this audit owns per-crate build cost in one build (dev, release, and
incremental). The [CI workflow audit](/quest/m2/audit-ci.md) owns job-level CI
cost and caching; the [dependency audit](/quest/m2/audit-deps.md) owns
deduplicating and upgrading dependencies.

What to check first:

- Keep fat LTO under the measured shipping-profile comparison below. A
  faster final link alone did not make the whole build cheaper. Revisit if
  the dependency graph or shipping profile changes substantially.
- Rerun the six-target IR inventory and the cold dev/release timings below.
  Attribute a large row to a current consumer before changing its types or
  splitting a crate. The off-cycle run rejected its reader-consolidation
  candidate; it found no unused whole crate or feature that could be removed
  without changing a public feature.
- Keep feature-unification and cross-job cache work with
  [moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985), and test work
  with [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987) and the
  [test cost audit](/quest/m2/audit-test-cost.md). Dependency and manifest
  removal belongs to the [dependency audit](/quest/m2/audit-deps.md).
- Transport-session duplication is fixed by
  [moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176). At this run's
  source, moq-tokio emits 556,292 lines and the old transport-row definition
  matches 401 lines (0.07%), against 1,679,222 and 39.1% in the historical
  table. Do not reintroduce generic session bodies or redo poll-first's
  async-feature work.
- moq-net's BTree rows remain large, but sharing numeric keys was measured
  and rejected: [the retained experiment](/.github/btree-build-audit.md)
  found about 30 ms of CPU per build, with no CPU-time reduction beyond
  noise, at the cost of key translation. A large aggregate share alone is
  not evidence for that refactor.
- Do not repeat the bare-slice-to-cursor MP4 experiment without checking
  mp4-atom's own decoders first. At 0.17.0 its `Atom::decode_atom` feeds
  nested bodies through a bare slice, while `Any::decode_atom` feeds them
  through `&mut &[u8]`. Both variants remain needed even when every owned
  reader uses a cursor. The measured candidate is rejected below.
- `cargo-llvm-lines` 0.4.48 is still outside the pinned shell. This run used
  `nix shell nixpkgs#cargo-llvm-lines` from inside the repository's Nix shell;
  no flake or dependency changes were made alongside the concurrent work.
- Measure edits through mbx, which enables learned incremental compilation;
  a plain Cargo rebuild with the host's `incremental = false` is a different
  experiment. [moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594)
  tracks mbx's E2BIG failure. Preserve that failure when validating; disabling
  the wrapper for a controlled compilation measurement is not a gate pass.
  On the hybrid host, use `perf stat --no-scale` and sum the unscaled core
  counters. Scaled counts can overcount by up to 2x.

### Off-cycle checkpoint (2026-11-01)

Base: [21d6da9fc1](https://github.com/moq-dev/moq/commit/21d6da9fc108b48373a961f34e43c3fcd38b0146).
The one-off [build-time removal quest](/quest/m1/prune-build-time.md) has not
finished: local validation cannot launch Cargo through mbx (E2BIG), and the
mbx incremental probe and local review remain open. The shipping-profile
LTO comparison is complete below.
No production change is retained at this checkpoint. The recurring audit's
schedule is unchanged and no successor is created.

[Measurements and rejected candidate patches](/.github/compile-time-audit.json)
record the source, commands, top IR rows, Cargo unit durations, and raw perf
counters. Rust 1.98.1 and cargo-llvm-lines 0.4.48, Linux x86_64, Intel Core
Ultra 7 270K Plus (24 logical CPUs), shared host. Measurements use private
`.tmp/` targets, two Cargo jobs, `MBX_DISABLE=1`, and no incremental
compilation. The host overrides dev debug info to false. These are controlled
compilation measurements, not passing runs of the repository's gate.

| Target, default features, dev | LLVM lines | Copies |
| --- | ---: | ---: |
| moq-tokio lib | 556,292 | 21,364 |
| moq-mux lib | 1,144,450 | 34,033 |
| moq-net lib | 1,383,588 | 42,863 |
| moq-relay lib | 693,285 | 26,066 |
| moq-ffi lib | 798,830 | 31,542 |
| moq-cli `moq` bin | 1,087,790 | 39,583 |

Method: `cargo llvm-lines --locked -p <crate> --lib` (CLI: `--bin moq`).
The corresponding mux test-profile target (`--lib --profile test`) emits
3,726,367 lines in 103,615 copies. Counts are deterministic at this source
and feature set; code and feature changes since October make cross-date
counts context, not an attribution of all changes to one PR.

| Cold build | Wall | Process-tree CPU | Sum of unit wall durations | Longest units |
| --- | ---: | ---: | ---: | --- |
| `cargo build --locked --timings`, default members | 241.79 s | 460.12 s | 481.79 s | aws-lc-sys script 43.52 s; moq-net 12.66 s; moq-mux 9.86 s |
| `cargo build --locked --release -p moq-cli -p moq-ffi --timings` | 527.08 s | 927.04 s | 957.90 s | `moq` 224.57 s; moq-ffi 135.04 s; aws-lc-sys script 69.68 s |

Each is one observation on a shared host. Unit durations overlap and are not
CPU attribution. The dev build's scripts total 107.78 unit seconds (22.4%);
aws-lc-sys alone is 9.0%. The two final release units account for 37.5% of
unit time. Default-feature release artifacts are 46,726,912 bytes (`moq`)
and 65,005,970 bytes (`libmoq_ffi.a`). This is not the shipping Go mirror
measurement: it does not enable OpenH264.

Owned scripts in the default build are small: moq-sock compiles in 1.10 s,
moq-video in 1.05 s, and each runs in 0.01 s. Their platform aliases still
have consumers. kio-macros takes 1.19 s; its generated wrappers and machines
have live poll/async consumers. Retain those rather than duplicate platform
conditions or the state machines by hand. C's cbindgen and GStreamer's
version helper serve their exported header and plugin metadata and are not
in default members. The large generated UniFFI and configuration parsers
also have current consumers; removing them would remove public features.
No such removal is proposed here.

#### Rejected reader consolidation

The historical subtitle candidate is stale at mp4-atom 0.17.0. Replacing its
two bare slices with `Cursor<&[u8]>` changes mux lib IR from 1,144,450 to
1,144,440 lines and test IR from 3,726,367 to 3,726,357: only ten lines,
with the large decoder copies still present. Extending that candidate to
the streaming importer's `Moov::decode_body` and `Moof::decode_body` keeps
the lib at 1,144,440 but grows the test target to 3,727,271 (+904).

The crate's own `Atom::decode_atom` and `Any::decode_atom` introduce the two
slice forms while decoding nested bodies. Our top-level readers cannot
remove either generated decoder variant. Changing the dependency belongs to
the dependency audit, not an inline vendor patch here.

Plain mux-only dev rebuilds, dependencies warm, baseline/candidate/candidate/
baseline order, with both owned reader changes:

| Sample | Variant | Unscaled user instructions | Task clock |
| --- | --- | ---: | ---: |
| 1 | baseline | 53.271 G | 6.665 s |
| 2 | candidate | 53.281 G | 6.676 s |
| 3 | candidate | 53.265 G | 6.606 s |
| 4 | baseline | 53.275 G | 6.583 s |

Method: restore both source snapshots before each rebuild, then
`perf stat --no-scale -e instructions:u,task-clock cargo build --locked
-p moq-mux --lib --timings`. Sum the hybrid host's unscaled instruction
counters. The source patch and four raw counter outputs are retained in the
JSON. Instructions are flat and CPU times overlap; no speedup is established.
Both reader edits were dropped. Incomplete release rebuilds of the rejected
candidates were stopped and are excluded from the results.

#### Shipping-profile LTO comparison

Same unchanged source and the same 474 package/target/feature units, two
jobs, `cargo build-ffi --release --timings`, then the same command with
`CARGO_PROFILE_RELEASE_LTO=thin`. The alias enables OpenH264, as the shipped
FFI does. Each profile builds into the private measurement target with mbx
disabled. The set of timed units is identical, so the smaller final unit
must be read alongside its dependencies, not as the build's total saving.

| Metric | Fat | Thin |
| --- | ---: | ---: |
| Whole command wall | 270.39 s | 321.67 s |
| Whole command process-tree CPU | 390.19 s | 547.74 s |
| Sum of unit durations | 402.46 s | 539.86 s |
| Final moq-ffi unit | 135.70 s | 88.04 s |
| `libmoq_ffi.a` | 65,447,812 bytes (62.4 MiB) | 74,914,524 bytes (71.4 MiB) |

Thin cuts the final unit's wall by 35.1%, but did not speed up the whole
build in this run. Both archives fit the existing 100 MB per-file ceiling.
These are single observations on a shared host, not a general CPU regression
estimate or a cross-job cache measurement.

Runtime check: build the existing `moq-net` session bench once with each
profile, then run fat/thin/thin/fat with `SESSION_CPU=1`, filtering
`session_delivery_group_cost`, 0.1 s warm-up, 0.2 s requested measurement,
and ten samples per case. All 72 cases complete; they sweep publishers and
viewers independently across SUBSCRIBE/FETCH and the supported protocols.
The geometric mean of each case's mean thin CPU divided by mean fat CPU is
1.034; individual ratios range from 0.747 to 1.403. The repeated fat runs'
median max/min ratio is 1.020 and the largest is 1.966. Host variation is
substantial, so this does not prove a 3.4% runtime regression. It establishes
no runtime win that would justify reversing the retained fat-LTO choice.

Decision made in this run: keep fat LTO, consistent with the maintainer's
2026-10-20 choice. The archive is smaller now, but changing the profile still
needs an overall build benefit worth its runtime and size tradeoffs. No
profile edit is retained. The old thin-LTO open item is measured here;
the transport IR inventory and stale subtitle candidate are also resolved.
`cargo-llvm-lines` is still not in the pinned shell.

#### Remaining work

- Measure a real edit through mbx after
  [moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594) is resolved.
  The controlled whole-crate rebuilds above do not measure learned
  incremental compilation or cross-job CI cache reuse.
- Run the repository's checks and the other-vendor local review without
  bypassing E2BIG or the runner-image user-namespace denial. The targeted
  `just rs test -p moq-mux --lib` attempt failed in mbx before compilation;
  cargo-llvm-lines compiled the test harness but ran no tests.

### Historical baselines (m1 run, 2026-10-23)

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
  function would move them, but adds code for little. The catalog JSON diff
  no longer instantiates per crate since
  [moq-dev/moq#7400](https://github.com/moq-dev/moq/pull/7400): the encoder
  diffs `serde_json`'s bytes, compiled once in `moq-json`.
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
