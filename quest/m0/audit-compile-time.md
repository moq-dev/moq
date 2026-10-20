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

Measured 2026-10-19 on `cdff3c8f9` in the pinned Nix shell (Rust 1.98.1,
LLVM 22.1.8, Linux x86_64, Ryzen 9 5900XT, 32 logical CPUs), with Cargo,
native build tools, and Nextest bounded to two jobs. These are single
observations, not regression limits or evidence of an optimization.

Scope: this audit owns per-crate build cost in one build (dev, release, and
incremental). The [CI pipeline audit](/quest/m0/ci/audit.md) owns job-level CI
cost: cache keys and hits, aws-lc-sys recompiling several times per cold job,
both rustls backends in the gate, and `--timings` output in CI. Deduplicating
or upgrading dependencies belongs to the
[dependency audit](/quest/m1/audit-deps.md).

Clean builds used `MBX_DISABLE=1` and a fresh `CARGO_TARGET_DIR`, with registry
sources and the Nix shell already present; no cache, compiler, profile, or
feature setting was changed. The dev build ran on a quiet host (one-minute load
0.66 to 2.65); release and later runs shared it with bounded builds in other
worktrees (load up to 6.14). Do not compare their wall times as a before/after
result. Times exclude Nix startup.

| Experiment | Process wall time | Result |
| --- | ---: | --- |
| `cargo build --locked --timings` | 274.015 s | Passed; Cargo reported 4m32s |
| `cargo build --locked --release --timings` | 821.054 s | Passed; Cargo reported 13m40s |
| Unchanged default-member dev build | 0.497 s | Passed |
| Warm exact `cargo build --locked -p moq-net --timings` graph | 21.195 s | Passed; setup for the next two runs |
| Unchanged standalone moq-net build | 0.251 s | Passed |
| Standalone moq-net rebuild after a comment-only edit | 8.025 s | Passed; trivial-edit baseline only |
| Ordinary cache-enabled `just check` on that moq-net edit | 842.849 s | Failed at formatting; not a successful full-check baseline |

The default-member dev build compiled 681 Cargo units from 545 distinct
package/version pairs. Longest observed units, in seconds:

| Cargo unit | Dev | Release |
| --- | ---: | ---: |
| moq-cli binary (`moq`) | 14.87 | 342.30 |
| moq-relay binary | 5.13 | 191.80 |
| aws-lc-sys build-script execution | 87.59 | 102.69 |
| moq-boy binary | 3.74 | 101.72 |
| moq-bench binary | 3.76 | 83.95 |
| moq-tokio library | 17.83 | 50.77 |
| moq-relay library | 9.97 | 44.27 |
| moq-mux library | 8.55 | 30.46 |
| str0m library | 9.39 | 24.35 |
| yuv library | 10.75 | 23.07 |
| moq-net library | 10.01 | 21.06 |

These are elapsed times for overlapping units, not additive critical-path or
CPU times. Final release binary work dominates the release observations, but
these timings do not separate monomorphization, optimization, LTO, and linking.
The existing release profile uses fat LTO and one codegen unit to keep shipping
artifacts compact; no profile change is justified by this run alone.
aws-lc-sys's build script is the largest dev unit; keep the crypto provider
unchanged unless a separately justified decision permits it.

The largest dev proc-macro unit was usage-derive at 6.02 seconds; serde_derive
was 2.61 seconds. Fifty-one compiled dependency names had multiple versions;
the three syn versions took 2.99, 2.41, and 2.53 seconds. The default build
omitted iroh, wgpu, cpal, and OpenH264; it did compile str0m, object_store, and
the NVIDIA crates. Do not disable supported default functionality merely to
make a build smaller.

The cache-enabled net-edit `just check` used mbx 1.23.0 (820 hits, 72 misses,
seven units not looked up, four bypasses) and selected 28 Rust packages,
including downstream C, FFI, GStreamer, and WASM crates. Test compilation took
7m46s; all 7,608 native tests passed, with 31 skipped, in 369.431 seconds. It
then failed `cargo fmt --all --check` on the disposable probe, so rustdoc and
later feature checks never ran. `mbx explain --last` had no recorded build, so
those misses are unattributed.

Raw logs and Cargo timing reports were local only; re-measure rather than
trusting anything beyond these tables. `cargo llvm-lines` was unavailable in
the pinned shell.

Remaining work before completing this run:

1. Record a successful ordinary net-edit `just check` with a formatted probe,
   preserving its exact diff, selected packages, cache counters, and host load.
2. Attribute final release binary work with a supported LLVM-lines tool and
   compiler/linker profiling before choosing a refactor. A thin-LTO experiment
   must compare shipping sizes and relevant runtime workloads on the same
   source; changing the release profile is a maintainer decision.
3. Fix or settle the measured bottlenecks, then create the recurring m1
   successor as required above. The nightly size job already records release
   `Build` durations without comparing noisy hosted times (`sh/size/README.md`).

## Related

- [CI pipeline audit](/quest/m0/ci/audit.md) - owns job-level CI build cost and caching
- [Dependency audit](/quest/m1/audit-deps.md) - owns deduplicating and upgrading dependencies
