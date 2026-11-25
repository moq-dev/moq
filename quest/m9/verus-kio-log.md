# [L] Spike: Verus proves kio::log's commit protocol as written

## Goal

The core of `kio::log`'s lock-free commit protocol, written in Verus's
`verus!` dialect in place, is proven by Verus: a reader that finds an index
below the committed end reads a fully written entry, a slot never moves while
it is live, and eviction never frees an entry a pinned reader can still
reach. `cargo build` without Verus still builds kio unchanged for callers, and
the proof checks in a CI job. The PR reports what the dialect cost:

- proof lines per line of code;
- kio's plain build time and kio's instruction benches, before and after;
- how a contributor without Verus edits the ported code, and what breaks the
  proof check when they do;
- Verus's check time.

It ends by recommending, from those numbers, whether to port the rest of
kio's unsafe code (`channel.rs`, `task.rs`, `clock/runtime.rs`) and then
moq-net's lite-07 subscription group accounting, which the TLA+ model in
`rs/moq-net/verify/subscribe/` checks only as a design, and plans that work as
follow-up quests or drops it.

## Plan

Decided 2026-11-25 (maintainer, /quest-plan from a brainstorm of outlandish
improvements): ✅ Verus proofs of kio first, then subscription accounting if
kio's proof cost is acceptable (rejected: both now in parallel; kio only;
drop it, leaving TLA+, Kani, and Miri). ✅ Spike in the dialect first and
decide the rest from its numbers (rejected: port kio now without a spike; a
verified shadow copy differential-tested against kio, which can drift from
the code the way the proof is meant to prevent).

The planning round named `kio::lock` as the spike; it turned out to be a thin
`Arc<Mutex<T>>` wrapper whose only `unsafe` is its `Send` and `Sync` impls,
so it would measure nothing about lock-free proofs. `kio::log` holds 38 of
kio's `unsafe` uses and its atomics, so the spike moved there (2026-11-25,
same session; the maintainer can redirect it in review).

Verus publishes `vstd` and `verus_builtin` on crates.io, so kio stays
publishable. `verus!` erases ghost code for plain builds. Atomics need Verus's
atomic invariants or tokenized state machines; trust `crossbeam_epoch`'s
reclamation as a stated assumption (`external_body` with a spec) rather than
proving crossbeam. If proving the whole commit protocol is out of reach,
prove the slot publication alone and say so: a partial proof that measures the
cost still answers the spike's question.

Pin Verus through the dev shell (Nix), not a global install. Runs alongside
[Miri's GenMC mode](/quest/m5/miri-genmc.md), which explores the same code's
schedules without changing it; the PR says what each catches that the other
misses.

## Related

- [Kani proves the varint codecs](/quest/m4/codec-kani.md) - the bounded-proof tool already planned for moq-net's codecs
- [Miri's GenMC mode explores every schedule of kio's lock-free code](/quest/m5/miri-genmc.md) - the unchanged-code alternative for the same module
