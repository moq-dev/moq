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
incremental). The [CI workflow audit](/quest/m1/audit-ci.md) owns job-level CI
cost and caching; the [dependency audit](/quest/m1/audit-deps.md) owns
deduplicating and upgrading dependencies.

What to check first: whether [moq-net compiles once](/quest/m1/net-erase-transport.md)
landed. If so, rerun `cargo llvm-lines` on the moq-tokio lib and compare the
transport share below. Then whether `cargo-llvm-lines` joined the pinned shell.

### Baselines (m0 run, 2026-10-19 and 2026-10-20)

Pinned Nix shell (Rust 1.98.1, LLVM 22.1.8), Linux x86_64, Ryzen 9 5900XT,
32 logical CPUs, Cargo, native tools and Nextest bounded to two jobs. The host
was shared with other builds; load is the one-minute average. Only the
llvm-lines counts are deterministic. Every time is load-sensitive and a single
observation: compare shares within a run, never seconds across runs.

Clean builds (`MBX_DISABLE=1`, fresh target dir, registry and shell present):

| Build | Source | Load | Wall | Longest unit |
| --- | --- | --- | ---: | --- |
| `cargo build --locked --timings` (default members) | `cdff3c8f9` | 0.7 to 2.7 | 274 s | aws-lc-sys build script 87.6 s |
| `cargo build --locked --release --timings` (default members) | `cdff3c8f9` | up to 6.1 | 821 s | moq-cli bin 342 s |
| `cargo build --locked --release -p moq-cli --timings` | `d55407256` | 21 to 89, median 55 | 1,213 s | moq-cli bin 644 s |

The final `moq` binary unit was 42% and 53% of those two release walls. It is
serial: fat LTO with one codegen unit.

`cargo llvm-lines` 0.4.48 (`nix shell nixpkgs#cargo-llvm-lines`), dev profile,
default features, on `d55407256`:

| Target | LLVM IR lines | Copies | Notable share |
| --- | ---: | ---: | --- |
| moq-tokio lib | 1,661,503 | 54,069 | moq-net code generic over the transport session 43.2%; usage-rs derive 5.2% |
| moq-cli `moq` bin | 1,037,734 | 41,758 | tokio 30.1%; usage-rs derive 2.8% |
| moq-mux lib | 908,532 | 25,751 | serde 19.4%; mp4-atom `Any::decode_atom` over four reader types 4.2% |
| moq-relay lib | 711,268 | 27,162 | serde 28.0% (TOML config); usage-rs derive 8.6% |
| moq-net lib | 686,443 | 22,660 | BTreeMap internals 24.1% |
| moq-relay bin | 384,813 | 15,113 | tokio 29.8% |

In moq-tokio the transport-generic share splits into
`transport::Session<qmux::Session>` and
`transport::Session<quic::web::Session>`, each again under moq-net's IETF
`ControlStreamAdapter`, so the IETF half compiles four times. Dev builds share
generics, so downstream crates pay little of it (0.3% of the relay lib).

`RUSTC_BOOTSTRAP=moq cargo rustc --locked --release -p moq-cli --bin moq -- -Z time-passes`
on `d55407256` (load 25 to 52, median 36; 429 s):

| Pass | Seconds | Share |
| --- | ---: | ---: |
| `LLVM_fatlto` | 317.8 | 74.1% |
| `LLVM_passes` | 98.9 | 23.1% |
| `codegen_crate` (monomorphization and IR emission) | 7.2 | 1.7% |
| everything else (expansion, type and borrow checking) | 3.9 | 0.9% |
| `link` | 1.2 | 0.3% |

Naming only the `moq` crate in `RUSTC_BOOTSTRAP` keeps build-script nightly
probes stable, so the dependency build is the ordinary one. Shares should hold
under load, since every pass is single-threaded CPU work in one process.

Incremental: ordinary cache-enabled `just check origin/main` with one
formatted comment line appended to `rs/moq-net/src/lib.rs`, fresh target dir,
mbx 1.25.0, on `d55407256` (load 20 to 44, median 34; 646 s, failed). It
selected 29 Rust packages (moq-net and every dependent, including C, FFI,
GStreamer and WASM). Phases: rustdoc 1m41s (678 hits, 76 misses, 34 bypassed),
WASM clippy 18.8 s, io-uring clippy 33.5 s, test build 5m40s (524 hits, 83
misses, 5 bypassed). Nextest started 7,890 tests and stopped after 663 on the
known [failover flake](/quest/m1/test-flakes-2/redundant-failover-drift.md).
Cargo rebuilds every dependent when an upstream crate's source changes at all,
even a comment, so the test build is the bulk of the compile cost.

### Results of the m0 run

- Planned: [moq-net compiles once](/quest/m1/net-erase-transport.md) removes
  the per-transport copies, the largest attributable dev IR (43% of moq-tokio)
  and a share of the release fat-LTO module.
- Explained, no action: the release `moq` unit is all LLVM backend work on one
  fat-LTO module; linking is 1 s. The profile keeps shipping artifacts compact
  for the Go mirror's 100 MB limit, and release builds are off the merge-queue
  gate. aws-lc-sys's build script is the longest dev unit; the crypto provider
  stays. usage-rs derive output is external code. serde visitors per
  deserializer are inherent to TOML config.
- Candidates for next run: mp4-atom's `Any::decode_atom` instantiated per
  reader type (38k lines in moq-mux, an upstream change to decode from one
  slice type); a thin-LTO experiment only if release builds join a PR gate,
  comparing shipping sizes and runtime on the same source.

### Method

Per target, `CARGO_TARGET_DIR=.scratch/target-lines CARGO_BUILD_JOBS=2
MBX_DISABLE=1 cargo llvm-lines --locked -p <crate> --lib` (or `--bin`), then
group rows by the first path segment after stripping crate hashes, and by
substrings for the transport session types. Release: `cargo build --release
-p moq-cli --timings` into its own target dir, then the time-passes command
above. Sample `/proc/loadavg` every 30 s during each run.
