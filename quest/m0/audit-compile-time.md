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

Measured 2026-10-19 on source `cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95`
(claim commit `b5b43d4db7f9a2107c16c0282587cbeb8fc17751` adds no source).
The source hash above is recorded in the measurement logs; use
`cdff3c8f9` when selecting this baseline. All experiments used the pinned Nix
shell: Rust 1.98.1, LLVM 22.1.8, Linux x86_64, Ryzen 9 5900XT with 16 physical
cores and 32 logical CPUs. Cargo, native build tools, and Nextest were bounded
to two jobs or threads. These are single observations, not regression limits
or evidence of an optimization.

Clean Cargo builds disabled mbx explicitly with `MBX_DISABLE=1` and used a new
worktree-local `CARGO_TARGET_DIR=.scratch/cold-target`. The dev and release
profiles both started without compiled artifacts for that profile; registry
sources and the Nix shell were already installed. No shared cache was deleted,
and no compiler, profile, feature, or build-metadata setting was changed. The
dev build had a quiet-host window; release and subsequent runs shared the host
with bounded builds in other worktrees. Do not compare their wall times as a
before/after result.

| Experiment | Process wall time | Result |
| --- | ---: | --- |
| `cargo build --locked --timings` | 274.015 s | Passed; Cargo reported 4m32s |
| `cargo build --locked --release --timings` | 821.054 s | Passed; Cargo reported 13m40s |
| Unchanged default-member dev build | 0.497 s | Passed |
| Warm exact `cargo build --locked -p moq-net --timings` graph | 21.195 s | Passed; setup for the next two runs |
| Unchanged standalone moq-net build | 0.251 s | Passed |
| Standalone moq-net rebuild after a comment-only edit | 8.025 s | Passed; trivial-edit baseline only |
| Ordinary cache-enabled `just check` on that moq-net edit | 842.849 s | Failed at formatting; not a successful full-check baseline |

Times exclude Nix startup. The first dev observer polled completion every two
seconds, so its process time includes up to two seconds of observation delay;
the remaining process times use a monotonic clock around process completion.
Sampled one-minute load ranged from 0.66 to 2.65 during dev, and 1.32 to 6.14
during release. The normal `just check` used mbx 1.23.0 without disabling its
cache; it reported 820 hits, 72 misses, seven units not looked up, and four
bypasses. It is a different cache and package-selection experiment from the
clean builds.

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

The largest dev proc-macro unit was usage-derive at 6.02 seconds; serde_derive
was 2.61 seconds. Fifty-one compiled dependency names had multiple versions.
The three syn versions took 2.99, 2.41, and 2.53 seconds in separate Cargo
units. Their reverse dependency graph is captured, but compatible versions
cannot be assumed interchangeable. The default build omitted iroh, wgpu,
cpal, and OpenH264; it did compile str0m, object_store, and the NVIDIA crates.
Do not disable supported default functionality merely to make a build smaller.

The ordinary net-edit check selected 28 Rust packages including downstream C,
FFI, GStreamer, and WASM crates. Test compilation took 7m46s; all 7,608 native
tests passed, with 31 skipped, in 369.431 seconds. It then stopped at
`cargo fmt --all --check`: the disposable comment probe had an extra blank
line after the crate attribute. The probe was fully restored and is absent
from the PR. No SLOW, FAIL, or TIMEOUT test verdict occurred. Rustdoc and later
feature checks were not reached, so this run proves neither a complete
`just check` pass nor its successful wall-time baseline.

Raw process records, load samples, logs, probe patch, resolved dependency graph,
and named Cargo HTML timing reports are retained in this run's worktree under
`.scratch/`. HTML reports were copied to `.scratch/evidence/timings/` before
discarding the isolated build artifacts. `cargo llvm-lines` was unavailable in
the pinned shell. `mbx explain --last` returned no recorded build with session
events enabled, so this run does not attribute cache misses to `COMMIT_ID` or
any other unverified cause. No global cache configuration was changed.

Remaining work before completing this run:

1. Record a successful ordinary net-edit `just check` with a formatted probe,
   preserving its exact diff, selected packages, cache counters, and host load.
2. Attribute final release binary work with a supported LLVM-lines tool and
   compiler/linker profiling before choosing a refactor. A thin-LTO experiment
   must compare shipping sizes and relevant runtime workloads on the same
   source; changing the release profile is a maintainer decision.
3. Investigate avoidable repeated aws-lc-sys native compilation with actual
   cache-miss evidence. Keep the crypto provider and its behavior unchanged
   unless a separately justified decision permits changing them.
4. Fix or settle the measured bottlenecks, then create the recurring m1
   successor as required above. The nightly size job already records release
   `Build` durations without comparing noisy hosted times (`sh/size/README.md`).
