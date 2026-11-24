# [M] Every counted bench repeats its instruction count

## Goal

Each benchmark `just rs instructions` counts gives the same count for the same code, run after run, at any host load, and in any checkout, so a Kixel Quest bisect step or a local `compare` reports only real changes.
A bench whose count cannot be made repeatable leaves `sh/rs/instructions.sh`'s selection, with the reason in its comment; Nightly keeps timing it.

## Plan

Measured 2026-11-24 on rust2-bench at kixel-ai/rust2@6ead6d56 (fingerprint `x86_64-3059cd1d9e65`), with no code change between runs:

- Two full runs in one checkout, each running 32 Valgrind jobs at once on a host other agents also loaded, agreed exactly on 527 of 1,284 benchmarks, with a median change of 0. 39 moved over 1%, from `rs/moq-mux/benches/containers.rs` (10), `rs/moq-net/benches/stats.rs` (7), `retirement.rs` (7), `rs/moq-mux/benches/catalog.rs` (5), `rs/moq-net/benches/announce.rs` (5), and a few others. The worst, `rs/moq-net/benches/auth.rs::benches::waiting::auth/waiting/moq-lite-07-wip::128[128]`, read 70,825,598 and 93,382,371 (32%).
- Counted one at a time (`--jobs 1`), the same bench read 70.80M, 70.83M, and 70.91M, and the others in that sample moved under 1%. Disabling ASLR (`setarch -R`) steadied nothing. CodSpeed, which counted each benchmark in turn on a hosted runner, read that bench at 75.31M to 75.38M across 20 runs. So the counts depend on how fast the host runs them: `auth` drives a tokio runtime with its timers enabled, which reads the wall clock.
- Between a checkout and `compare`'s base worktree under `~/.cache/rust2-bench`, some counts that repeat exactly within one checkout differ by location alone, such as `rs/moq-mux/benches/jitter.rs::benches::handoff::jitter_handoff::4[64]` (459,450 in one, 486,025 in the other) and `rs/moq-json/benches/codec.rs::benches::marshal::marshal::full[big_static]` (170,198 against 169,680).

Recommendations for the implementer:

- Count one noisy benchmark a few times with `--benchmarks`, at `--jobs 1` and at full load, and diff Callgrind's per-function costs to find what differs: timer and clock reads, thread interleaving, `RandomState` hash seeds, or a path or environment size that shifts alignment for glibc's string functions.
- Fix it in the bench where the bench controls the source, such as a runtime on kio's simulated clock, tokio's paused time, or a fixed hasher. A location dependence that only `compare`'s separate base worktree causes belongs to rust2-bench instead.
- Prove it by counting every benchmark twice at full load, in two checkouts at different paths, and finding every count equal.

Public API: none. Wire: none.
