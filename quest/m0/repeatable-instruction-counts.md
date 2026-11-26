# [M] Every counted bench repeats its instruction count

## Goal

Each benchmark `just rs instructions` counts gives the same count for the same code, run after run, at any host load, and in any checkout, so a Kixel Quest bisect step or a local `compare` reports only real changes.
A bench whose count cannot be made repeatable leaves `sh/rs/instructions.sh`'s selection, with the reason in its comment; Nightly keeps timing it.

## Plan

Decided 2026-11-24 (maintainer, pick round on [moq-dev/moq#9555](https://github.com/moq-dev/moq/pull/9555)): ✅ keep this quest and move it to `m0`, since noisy series now feed Kixel Quest's regression filing and bisects (rejected: drop it; keep it in `m9`). ✅ land rust2-bench counting every target meanwhile (rejected: leave the 11 noisy targets out until they repeat): "land now, we don't want hosted runners eventually anyway". Superseded 2026-11-26 by the org's runner rule, which lets a public repository's jobs outside merge gates, such as these Nightly and bisect counts, stay on GitHub-hosted runners, so no move off them is planned. A fix must still hold in any checkout, as the Goal says, not only on `ubuntu-24.04`.

Measured 2026-11-24 on rust2-bench at kixel-ai/rust2@6ead6d56 (fingerprint `x86_64-3059cd1d9e65`), with no code change between runs:

- Two full runs in one checkout, each running 32 Valgrind jobs at once on a host other agents also loaded, agreed exactly on 527 of 1,284 benchmarks, with a median change of 0. 39 moved over 1%, from `rs/moq-mux/benches/containers.rs` (10), `rs/moq-net/benches/stats.rs` (7), `retirement.rs` (7), `rs/moq-mux/benches/catalog.rs` (5), `rs/moq-net/benches/announce.rs` (5), and a few others. The worst, `rs/moq-net/benches/auth.rs::benches::waiting::auth/waiting/moq-lite-07-wip::128[128]`, read 70,825,598 and 93,382,371 (32%).
- Counted one at a time (`--jobs 1`), the same bench read 70.80M, 70.83M, and 70.91M, and the others in that sample moved under 1%. Disabling ASLR (`setarch -R`) steadied nothing. CodSpeed, which counted each benchmark in turn on a hosted runner, read that bench at 75.31M to 75.38M across 20 runs. So the counts depend on how fast the host runs them: `auth` drives a tokio runtime with its timers enabled, which reads the wall clock.
- The hosted runner shares this fingerprint, yet its first full count ([run 45488316190](https://github.com/moq-dev/moq/actions/runs/45488316190), 4 jobs on 4 vCPUs, in `/home/runner/work/moq/moq`) agreed exactly with a local count on only 286 of 1,284 benchmarks: median 0.05% apart, 100 over 1%. It read `auth/waiting/moq-lite-07-wip::128[128]` at 93,615,691. So a local count does not yet reproduce CI's.
- Two hosted runs of the same code (runs 45488316190 and 45494124465, 4 jobs on 4 vCPUs) agreed exactly on 600 of 1,284 benchmarks; 47 moved over 1%, the worst `auth/waiting/moq-lite-07-wip::1[128]` by 26%. Even the same tiny binary moves: `rs/moq-net/benches/varint.rs::benches::bench::lite_varint::decode[Audio/moq-lite-06]` read 1,495 in a full local run and 1,395 counted alone. The workflow now counts one benchmark at a time (`--jobs 1`) so nightly and bisect steps share a load, but that did not steady them on a hosted runner: two `--jobs 1` runs naming those 47 (runs 45498389383 and 45498403359) agreed exactly on 7, and 19 still moved over 1%. `auth/waiting/moq-lite-07-wip::1[128]` reads either about 63.5M or about 86.0M under both settings, so the spread is inside the benches. The 47 sit in 11 targets that hold 490 of the 1,284 benchmarks; most in `stats` (9), `containers` (8), `retirement` (8), `catalog` (5), and `announce` (5).
- Between a checkout and `compare`'s base worktree under `~/.cache/rust2-bench`, some counts that repeat exactly within one checkout differ by location alone, such as `rs/moq-mux/benches/jitter.rs::benches::handoff::jitter_handoff::4[64]` (459,450 in one, 486,025 in the other) and `rs/moq-json/benches/codec.rs::benches::marshal::marshal::full[big_static]` (170,198 against 169,680).

Recommendations for the implementer:

- Count one noisy benchmark a few times with `--benchmarks`, at `--jobs 1` and at full load, and diff Callgrind's per-function costs to find what differs: timer and clock reads, thread interleaving, `RandomState` hash seeds, or a path or environment size that shifts alignment for glibc's string functions.
- Fix it in the bench where the bench controls the source, such as a runtime on kio's simulated clock, tokio's paused time, or a fixed hasher. A location dependence that only `compare`'s separate base worktree causes belongs to rust2-bench instead.
- Prove it by counting every benchmark twice at full load, in two checkouts at different paths, and finding every count equal.

Public API: none. Wire: none.
