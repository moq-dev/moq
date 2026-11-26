# [L] Spike: #[kio::reactor] guarded-action macro with sim action explorer

## Goal

An internal prototype in `kixel-ai/kio` extends `kio-macros` with a TLA+-style reactive macro (`#[kio::reactor]`, evolving `#[kio::machine]` from linear `wait!` chains to named guarded actions `Next == A \/ B \/ C` over shared `kio` channels and `Waiter`s). The same source compiles two ways:

- In normal builds (`not(feature = "sim")`): a zero-overhead deterministic priority-ordered poll loop with no extra allocations.
- Under `sim`: each `#[action]` is exposed individually so the simulator can fire enabled actions nondeterministically (exposing hidden priority-ordering assumptions), check `#[invariant]` methods after every action step, re-probe parked actions with `Waiter::noop()` to flag lost wakeups by action name, and emit structured action traces suitable for TLC trace validation.

The spike implements `SendCredit` (using real `kio` channels, `Waiter`s, and `Line::wake_head`) with `#[kio::reactor]`, validates emitted traces against `SendCredit.tla`, and reports macro ergonomics/LOC, instruction counts vs. hand-written `poll_*` code, injected ordering and lost-wakeup bugs caught, and compile-time / error-message quality.

## Plan

Part of [Spike provable state-machine designs for kio](/quest/m9/kio-state-machines/README.md). Carried into `kixel-ai/kio` when [kio-repo](/quest/m1/kio-repo.md) lands.

- Build on the existing `kio-macros` crate (`rs/kio/macros/src/lib.rs`, which already provides `#[kio::machine]` and `#[kio::wrapper]`), keeping `#[kio::reactor]` internal/experimental until the epic's comparison report settles the design.
- Keep the action contract explicit and small: an action returning `Poll::Pending` means its guard is disabled (and registered its waiter); returning `Poll::Ready` means one atomic transition occurred. Check whether actions can remain pure of hidden blocking or re-entrant locks.
- Test trace validation end-to-end on `SendCredit` by writing a thin projection from the macro's emitted action log into TLC trace validation against `SendCredit.tla`, measuring how much glue code remains compared to hand-instrumenting `poll_*` functions.
- Inject at least two real failure modes (a missing `wake_head` wakeup and a priority-dependent `give_way` / preemption race) and confirm the `sim` action explorer catches both with named action traces.

Public API: none. Wire: none.
