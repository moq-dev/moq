# [L] Spike: pure kio StateMachine with built-in BFS checker, Kani, and Aeneas

## Goal

An internal prototype in `kixel-ai/kio` demonstrates a pure, synchronous control-plane `StateMachine` abstraction (`Clone + Eq + Hash`, `step(&mut self, event, out)`, `invariant()`) paired with a thin `kio` async driver and a built-in explicit-state BFS model checker (visited-set deduplication and minimal counterexample traces). The spike implements `moq-net`'s `SendCredit` protocol (`SendCredit.tla`'s stream-credit line, preemption, `give_way`, and `stuck` cursor) on this abstraction, plus a small pure `WarmSettle` routing machine (`WarmSettle.tla`'s ranked warm-pull selection) as a Lean 4 benchmark fixture, and evaluates three verifiers directly on the Rust code:

1. `kio`'s built-in BFS checker on `SendCredit` against the same rules and configs as `SendCredit.tla` (`WithinCredit`, `Progress`/`Ends`, `TopDelivered`, plus the witnesses);
2. Kani bounded harnesses on `SendCredit`'s single-step invariants and panic-freedom;
3. Aeneas (Charon MIR -> Lean 4) extraction on the pure `SendCredit` and `WarmSettle` machines, attempting one nontrivial Lean 4 proof (such as `WithinCredit` or a `WarmSettle` ranking/settle lemma) to baseline against `kixel-ai/rust2`'s `rust2-lean` spike.

The PR reports LOC, ergonomics, BFS states/sec and memory vs. TLC, Kani check time, Aeneas extraction friction and Lean proof cost, and runtime instruction overhead of the `kio` driver shell vs. direct poll code.

## Plan

Part of [Spike provable state-machine designs for kio](/quest/m9/kio-state-machines/README.md). Carried into `kixel-ai/kio` when [kio-repo](/quest/m1/kio-repo.md) lands.

- Keep the `StateMachine` trait and BFS explorer internal (no published API changes yet). Model multi-actor composition (e.g., `Publisher` + `Line` + abstract `Transport` / `Wire`) as a small generic harness so BFS explores nondeterministic delivery, resets, and arrivals with state hashing and symmetry/canonicalization if needed.
- Measure the boundary cost honestly: show what happens when a control-plane machine must coordinate with shared data-plane state (e.g., passing opaque group keys rather than payload `Bytes`), and note any place where `Clone + Eq + Hash` forces awkward data structures (`BTreeMap` vs. `HashMap`, bounded arrays, or indices).
- For Aeneas, pin or run via Nix rather than a global install. If Aeneas refuses a construct (`BTreeMap`, `VecDeque`, traits), record the exact refusal and whether a small rewrite unblocks it: that friction table is a primary input to `kixel-ai/rust2`'s `rust2-lean` spike (`quest/m2/lean-spike.md` there).
- Verify the checker catches real bugs by injecting at least two regressions (e.g., dropping `give_way` so `SendCreditLive` breaks `TopDelivered`, and dropping a preemption reset) and showing the printed counterexample trace.

Public API: none. Wire: none.
