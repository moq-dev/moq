# [S] Cheaper parking and waking in kio

## Goal

Three overheads on kio's park/wake edge, paid on every hot channel:

- `WaiterList::wake` drains the list, so every parked consumer must
  re-register on its next poll, and every registration round-trips the
  `Identity::recorded` mutex (`Identity::presume`), a second lock per
  parked poll beside the channel's own.
- `kio::Tasks`' `Shared::wake_parent` takes a mutex to clone the parent
  waker on the first wake of each cycle, even though that waker is
  invariant for the life of the worker.
- A list holding more than `INLINE_WAITERS` (4) waiters reallocates its
  heap buffer on every take/wake cycle, so an 8-subscriber track allocates
  once per group.

Make steady-state park/wake free of the second mutex, the invariant-waker
clone, and the per-cycle realloc.

## Plan

- Persistent registration or an epoch fast path: a waiter whose identity and
  epoch are unchanged since its last registration skips `presume`'s lock
  entirely. The existing generational `(id, epoch)` dedup was built to make
  stale records provable; extend it so re-registration is a compare, not a
  lock.
- Store the parent waker in a write-once cell (it is set when the worker
  first polls and never changes), so `wake_parent` clones without locking.
  If a runtime ever legitimately swaps parents, keep the slow path behind
  the same seam.
- Reuse wake buffers: swap between two owned buffers on `take` instead of
  handing the allocation away, so fanout wakes stop allocating.
- All of this is waker soup: it must stay loom-clean (`just rs loom`) and
  respect the known loom `will_wake` behavior between polls. No semantic
  change to wake ordering or spurious-wake tolerance.

Acceptance: `rs/kio/benches/waiter.rs`, `channel.rs`, and `tasks.rs` before
and after, plus relay CPU at the fanout shape via `just bench BASE` on
Linux.
