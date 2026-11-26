# Spike provable state-machine designs for kio

## Goal

Prototype and compare three candidate state-machine designs inside `kixel-ai/kio` on a shared `SendCredit` fixture, alongside `kixel-ai/rust2`'s `rust2-lean` MIR-extraction spike on routing (`WarmSettle`), touching no published API, wire format, or CLI. Once all three `kio` spikes finish, this epic writes a single comparison report covering:

- ergonomics (lines of code, boilerplate, and readability compared to hand-written `poll_*` loops);
- verification and bug-finding power (interleavings covered, injected bugs caught, counterexample quality, and what each approach proves vs. bounded-checks vs. fuzzes);
- verification speed and scalability vs. TLC's `SendCredit` configs;
- build time and runtime instruction cost;
- how each approach composes with `kixel-ai/rust2`'s `rust2-lean` spike and with `moq`'s zero-copy fan-out data plane.

It ends by planning follow-up quests for the winning direction (or dropping the approaches that do not pay for their weight).

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` on designing async Rust for state-machine verification):

- ✅ Goal: prototype and compare candidate state-machine designs for `kio` on real `moq-net` protocol logic as internal lab spikes, leaving `kio::log` Verus proofs and `m4` TLA+ trace validation to their existing quests (rejected: narrowing to pure `StateMachine` only, narrowing to `kio`'s poll layer only, dropping the spikes).
- ✅ Repo home: move `rs/kio` to `kixel-ai/kio` in `m1` first, then run these three spikes in `kixel-ai/kio`'s quest tree; plan the fourth approach (`rust2-lean` MIR extraction) in `kixel-ai/rust2`'s quest tree (rejected: putting all spikes in `rust2`, a fresh throwaway repo, keeping the spikes in `moq`'s `m9` lab first, putting MIR extraction inside `kio`).
- ✅ Approaches: spike all four:
  1. [Pure `StateMachine` + BFS + Kani/Aeneas](/quest/m9/kio-state-machines/pure-machine.md);
  2. [`#[kio::reactor]` guarded-action macro](/quest/m9/kio-state-machines/reactor-macro.md);
  3. [Adversarial `kio::clock::sim` + `kio::Tasks` scheduler + lost-wakeup detector](/quest/m9/kio-state-machines/adversarial-sim.md);
  4. `rust2-lean` MIR extraction in `kixel-ai/rust2` (`quest/m2/lean-spike.md` there).
- ✅ Benchmark targets: `SendCredit` (`rs/moq-net/verify/credit/SendCredit.tla`, 289 lines: stream-credit line, preemption, `give_way`, `stuck` cursor, `Line::wake_head`) across all three `kio` spikes, plus routing (`WarmSettle` from `rs/moq-net/verify/warm/WarmSettle.tla` / `rs/moq-net/src/route/tests/warm.rs`) for the Lean 4 (`rust2-lean` vs. Aeneas) comparison (rejected: `Reconnect` / `Handover` for the `kio` spikes, `WarmSettle` across every spike).

Carried into `kixel-ai/kio`'s quest tree when [Move kio to its own repository](/quest/m1/kio-repo.md) executes, dropping the `Required` link below. Keep every prototype behind an internal/experimental feature or test crate until the comparison report settles the public API.

Public API: none (lab spikes only). Wire: none.

## Required

- [Move kio to its own repository, kixel-ai/kio](/quest/m1/kio-repo.md) - moves `kio` into its own workspace and carries this epic there

## Related

- [Spike: Verus proves kio::log's commit protocol as written](/quest/m9/verus-kio-log.md) - deductive proofs on `kio`'s unsafe lock-free internals
- [Subscription runs are checked against the TLA+ model](/quest/m4/subscribe-trace-validation.md) - external TLA+ trace validation planned in `moq`
- [Resume sweeps are checked against the TLA+ model](/quest/m4/resume-trace-validation.md) - shared trace format and TLC recipe in `moq`
- [Reconnect runs are checked against the TLA+ model](/quest/m4/reconnect-trace-validation.md) - external trace validation for `Handover.tla`
- [Kani proves the varint codecs and length arithmetic for every input](/quest/m4/codec-kani.md) - bounded code proofs for `moq-net` codecs
- [Miri's GenMC mode explores every schedule of kio's lock-free code](/quest/m5/miri-genmc.md) - weak-memory schedule exploration for `kio`
