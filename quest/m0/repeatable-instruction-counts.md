# [M] Every counted bench repeats its instruction count

## Goal

Each benchmark `just rs instructions` counts gives the same count for the same code, run after run, at any host load, and in any checkout, so a Kixel Quest bisect step or a local `compare` reports only real changes.
A bench whose count cannot be made repeatable leaves `sh/rs/instructions.sh`'s selection, with the reason in its comment; Nightly keeps timing it.

## Plan

Decided 2026-11-24 (maintainer, pick round on [moq-dev/moq#9555](https://github.com/moq-dev/moq/pull/9555)): ✅ keep this quest and move it to `m0`, since noisy series now feed Kixel Quest's regression filing and bisects (rejected: drop it; keep it in `m9`). ✅ land rust2-bench counting every target meanwhile (rejected: leave the 11 noisy targets out until they repeat): "land now, we don't want hosted runners eventually anyway". The counts are expected to move off GitHub-hosted runners, so a fix must hold on any host rust2-bench runs on, not only on `ubuntu-24.04`; that move belongs here or in a quest of its own.

Measured 2026-11-24 on rust2-bench at kixel-ai/rust2@6ead6d56 (fingerprint `x86_64-3059cd1d9e65`), with no code change between runs:

- Two full runs in one checkout, each running 32 Valgrind jobs at once on a host other agents also loaded, agreed exactly on 527 of 1,284 benchmarks, with a median change of 0. 39 moved over 1%, from `rs/moq-mux/benches/containers.rs` (10), `rs/moq-net/benches/stats.rs` (7), `retirement.rs` (7), `rs/moq-mux/benches/catalog.rs` (5), `rs/moq-net/benches/announce.rs` (5), and a few others. The worst, `rs/moq-net/benches/auth.rs::benches::waiting::auth/waiting/moq-lite-07-wip::128[128]`, read 70,825,598 and 93,382,371 (32%).
- Counted one at a time (`--jobs 1`), the same bench read 70.80M, 70.83M, and 70.91M, and the others in that sample moved under 1%. Disabling ASLR (`setarch -R`) steadied nothing. CodSpeed, which counted each benchmark in turn on a hosted runner, read that bench at 75.31M to 75.38M across 20 runs. So the counts depend on how fast the host runs them: `auth` drives a tokio runtime with its timers enabled, which reads the wall clock.
- The hosted runner shares this fingerprint, yet its first full count ([run 45488316190](https://github.com/moq-dev/moq/actions/runs/45488316190), 4 jobs on 4 vCPUs, in `/home/runner/work/moq/moq`) agreed exactly with a local count on only 286 of 1,284 benchmarks: median 0.05% apart, 100 over 1%. It read `auth/waiting/moq-lite-07-wip::128[128]` at 93,615,691. So a local count does not yet reproduce CI's.
- Two hosted runs of the same code (runs 45488316190 and 45494124465, 4 jobs on 4 vCPUs) agreed exactly on 600 of 1,284 benchmarks; 47 moved over 1%, the worst `auth/waiting/moq-lite-07-wip::1[128]` by 26%. Even the same tiny binary moves: `rs/moq-net/benches/varint.rs::benches::bench::lite_varint::decode[Audio/moq-lite-06]` read 1,495 in a full local run and 1,395 counted alone. The workflow now counts one benchmark at a time (`--jobs 1`) so nightly and bisect steps share a load, but that did not steady them on a hosted runner: two `--jobs 1` runs naming those 47 (runs 45498389383 and 45498403359) agreed exactly on 7, and 19 still moved over 1%. `auth/waiting/moq-lite-07-wip::1[128]` reads either about 63.5M or about 86.0M under both settings, so the spread is inside the benches. The 47 sit in 11 targets that hold 490 of the 1,284 benchmarks; most in `stats` (9), `containers` (8), `retirement` (8), `catalog` (5), and `announce` (5).
- Between a checkout and `compare`'s base worktree under `~/.cache/rust2-bench`, some counts that repeat exactly within one checkout differ by location alone, such as `rs/moq-mux/benches/jitter.rs::benches::handoff::jitter_handoff::4[64]` (459,450 in one, 486,025 in the other) and `rs/moq-json/benches/codec.rs::benches::marshal::marshal::full[big_static]` (170,198 against 169,680).

Found 2026-11-24 on [moq-dev/moq#9574](https://github.com/moq-dev/moq/pull/9574) (diffing Callgrind profiles of runs that disagree; the scratch shims are described there):

- Nearly every disagreement is the same code path hitting a different glibc malloc state (`_int_malloc` against a tcache hit), so any work before the measured iteration that varies moves the count. Three things vary it:
  - Random seeds: std's `RandomState` takes fresh `getrandom` bytes per process, so HashMap iteration and free order differ every run. A preloaded `getrandom` returning a fixed-seed stream (a constant one hangs QUIC connection-ID generation) steadied every `stats`, `retirement`, and `announce` count and raised full-load agreement from 527 to 900 of 1,284.
  - The host clock: the rest read it, through tokio's real clock or kio's std fallback outside a runtime (`ts_si_*` has no runtime at all). Adding a preloaded clock that advances 1µs per read made all 1,284 counts agree exactly across two runs at 32 jobs on a host already at load 32, `auth` included. Against today's counts, 53 benchmarks shift over 1% (worst `auth/waiting` at -26%), so adopting it starts new series.
  - Location: `nix develop` linked every binary with an RPATH to `<checkout>/outputs/out/lib`, and Cargo's registry paths under `$CARGO_HOME` land in panic locations, so the same code built a different binary per checkout and per host. This PR sets `NIX_NO_SELF_RPATH` in the dev shell and remaps `$CARGO_HOME` in `sh/rs/instructions.sh`: two checkouts at different paths with different `CARGO_HOME` paths now build byte-identical bench binaries. At run time the job's environment still differs by host (rust2-bench passes it whole), and its size moves the stack: padding the environment moved `jitter_handoff::4[64]` by up to 12 instructions.

What remains, in rust2-bench (kixel-ai/rust2), then a `rust2` bump here:

- Give each counted job deterministic entropy and a deterministic clock, covering every bench at once instead of fixing each source per bench; fold the change into the fingerprint so Kixel Quest starts new series instead of filing the shift as regressions; and fail a target that would bypass it (a static or non-glibc binary).
- Launch jobs with a canonical environment, cwd, and workspace-root path of fixed length, so the stack and the early heap match on every host.
- Prove it here by counting every benchmark twice at full load, in two checkouts at different paths, and finding every count equal; then drop the `--jobs 1` from `.github/workflows/codspeed.yml` and the wall-clock caveats in it and `sh/rs/instructions.sh`.

Public API: none. Wire: none.

## Waiting

- Maintainer: where the entropy and clock fix lives. Options: rust2-bench preloads a deterministic `getrandom` and clock into every counted job (recommended: one change covers every bench and future ones, on any host); or fix each bench's sources in moq (paused tokio or kio's simulated clock per bench), which cannot reach std's `RandomState` inside library code, so seeds would still need rust2-bench. Check: a kixel-ai/rust2 quest or PR owns it.
