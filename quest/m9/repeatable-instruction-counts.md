# [M] Every counted bench repeats its instruction count

## Goal

Each benchmark `just rs instructions` counts gives the same count for the same code, run after run and in any checkout, so a Kixel Quest bisect step or a local `compare` reports only real changes.
A bench whose count cannot be made repeatable leaves `just rs instructions`'s selection, with the reason in its comment; Nightly keeps timing it.

## Plan

Measured 2026-11-24 on rust2-bench at kixel-ai/rust2@6ead6d56 (fingerprint `x86_64-3059cd1d9e65`), with no code change between runs:

- Two full runs in one checkout agreed exactly on 527 of 1,284 benchmarks, with a median change of 0; 39 moved over 1%, from `rs/moq-mux/benches/containers.rs` (10), `rs/moq-net/benches/stats.rs` (7), `retirement.rs` (7), `rs/moq-mux/benches/catalog.rs` (5), `rs/moq-net/benches/announce.rs` (5), and a few others. The worst, `rs/moq-net/benches/auth.rs::benches::waiting::auth/waiting/moq-lite-07-wip::128[128]`, read 70,825,598 and 93,382,371 (32%).

- Across one checkout, most counts repeat, but some move from run to run: `rs/moq-net/benches/auth.rs::benches::waiting::auth/waiting/moq-lite-07-wip::128[16]` read 14,544,229, 14,531,071, and 17,097,964 in three runs, and `rs/moq-mux/benches/export.rs::benches::video_reorder::video-reorder::ts/step-back[256x16384]` read 36,324,509 and 37,964,104. Disabling ASLR (`setarch -R`) did not steady them.
- Between the checkout and `compare`'s base worktree under `~/.cache/rust2-bench`, 165 of 214 sampled benchmarks differed: median 0.05%, 18 over 1%, at most 17%. Some that repeat exactly within one checkout differ only by location, such as `rs/moq-mux/benches/jitter.rs::benches::handoff::jitter_handoff::4[64]` (459,450 here, 486,025 there) and `rs/moq-json/benches/codec.rs::benches::marshal::marshal::full[big_static]` (170,198 against 169,680).

Recommendations for the implementer:

- Count one noisy benchmark a few times with `--benchmarks` and diff Callgrind's per-function costs (the job directory a failed run keeps, or `callgrind_annotate`) to find what differs: thread interleaving, `RandomState` hash seeds, a clock read, or a path or environment size that shifts alignment for glibc's string functions.
- Fix it in the bench, such as a fixed hasher or one thread, when the bench controls the source; a location dependence only `compare`'s separate base worktree causes belongs to rust2-bench instead.
- Prove it by counting every benchmark twice in two checkouts at different paths and finding every count equal.

Public API: none. Wire: none.
