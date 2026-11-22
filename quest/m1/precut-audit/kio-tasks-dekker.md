# [S] Fix Dekker memory-ordering lost-wakeup race in kio::task

## Goal

`rs/kio/src/task.rs` cannot suffer a store-load reordering lost wakeup between
`pending` and `ready` on weakly ordered architectures or x86 store buffers.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ fix high-severity non-breaking bugs in `m1`
(rejected: `m0` with `release` backports; `m3`).

- `rs/kio/src/task.rs:145,297,504,525` pairs a `Release` store on one atomic
  (`pending` / `ready`) with an `Acquire` load on the other in a Dekker-style
  wake/sleep handoff. Under the C++20/Rust memory model (and on x86 TSO store
  buffers as well as ARM64), a `Release` store followed by an `Acquire` load to
  a *different* atomic can be reordered, letting both threads observe the other
  as idle and permanently losing a task wakeup.
- Upgrade the Dekker store/load pairs on `pending` and `ready` to `SeqCst` (or
  insert `std::sync::atomic::fence(Ordering::SeqCst)` between each store and
  the subsequent cross-variable load, documenting the pairing invariant).
- Audit the rest of `rs/kio/src/task.rs` for any related atomic handoff.

Public API: none. Wire: none. Run `rs/kio` tests (including stress tests and
miri if applicable) and document the memory-ordering proof in a concise comment
at the handoff.
