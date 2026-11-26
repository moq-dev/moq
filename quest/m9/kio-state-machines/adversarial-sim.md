# [M] Spike: adversarial kio::clock::sim and kio::Tasks scheduler with lost-wakeup detection

## Goal

An internal prototype in `kixel-ai/kio` upgrades `kio::clock::sim` and `kio::Tasks` so simulated runs explore task and child-slot interleavings adversarially rather than always polling `FuturesUnordered` and `Tasks` bitset slots in one fixed order, and automatically detects lost wakeups on unchanged poll code:

- A pluggable scheduler policy in `sim` + `kio::Tasks` supports randomized and PCT (probabilistic concurrency testing) task/slot ordering driven by `MOQ_SIM_SEED`, so multi-seed runs replay bit-exact and explore real scheduling races.
- At `idle` and before panicking on deadlock, a spurious-poll detector re-polls every parked root task, spawned task, and `kio::Tasks` child (tracking channel version bumps and `Poll::Ready` returns) and panics with the offending task and caller location if any parked task makes progress without having been woken.

The spike runs on a plain poll-based `SendCredit` harness (with injected wakeup and ordering bugs) plus `kio`'s own test suite, and reports catch rate, seeds-to-failure, runtime overhead per seed, any real bugs surfaced in `kio` itself, and whether stateful replay-from-prefix / DPOR adds practical value over randomized PCT at poll granularity.

## Plan

Part of [Spike provable state-machine designs for kio](/quest/m9/kio-state-machines/README.md). Carried into `kixel-ai/kio` when [kio-repo](/quest/m1/kio-repo.md) lands.

- Replace `sim::run_inner`'s opaque `FuturesUnordered` loop with an explicit ready-task set so the scheduler controls which ready task runs next, and hook `kio::Tasks::poll` under `feature = "sim"` so nested `Tasks` children participate in the same choice-point schedule instead of draining in fixed ascending slot order.
- Make sure zero overhead leaks into non-`sim` builds (`#[cfg(feature = "sim")]` only, verified with `kio`'s `tasks` and `channel` instruction benches).
- For lost-wakeup detection, be careful with timers and `idle()` futures (which legitimately stay pending until time jumps or quiescence) and with `kio::coop::Budget` yields: distinguish genuine progress (state mutation or `Ready` without time advancing) from innocent re-parking.
- Prototype a small bounded replay-from-prefix explorer (with optional channel-id read/write tracking for DPOR) on `SendCredit` and compare states explored and bugs found per second against randomized PCT, so the epic report has hard numbers on whether systematic replay is worth keeping in `kio`.

Public API: none. Wire: none.
