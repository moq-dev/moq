# [M] In-memory unsafe runs under Miri

## Goal

The hand-written unsafe in `kio` and `moq-net` that touches only memory (no
sockets, GPU, or FFI) runs under Miri nightly, so a use-after-free, data race,
or aliasing violation fails a test instead of waiting for production.

## Plan

From the 2026-10-27 coverage audit. No Miri job exists anywhere in the repo.
[Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) covers the
peer-reachable unsafe in `moq-c`, `moq-uring`, and `moq-sock`; this quest
covers the in-memory primitives it leaves out. `kio` has 33 `unsafe` sites
and `moq-net` 21 (2026-10-27), and Loom flagged concurrent `UnsafeCell`
access in kio's `a_log_reader_racing_eviction_never_touches_freed_memory`
that night, so start with kio's log and channels.

Decided 2026-10-27 (maintainer):

- Nightly only, never the merge gate: Miri is orders of magnitude slower than
  native tests.
- Run kio's unit tests whole, and the `moq-net` tests that reach its unsafe,
  selected by filter. Skip a test Miri cannot run (foreign calls, real
  sockets, wall-clock time) with `#[cfg_attr(miri, ignore = "...")]` naming
  why, not by narrowing the filter silently.
- Share one `just rs miri` recipe and nightly job with wire fuzz coverage:
  whichever lands first creates it, the other adds crates.
- Fix what it finds at the cause.

Public API: none. Wire: none.

## Related

- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - Miri for the peer-reachable unsafe
- [Nightly passes on main](/quest/m0/nightly/README.md) - the nightly this job joins
