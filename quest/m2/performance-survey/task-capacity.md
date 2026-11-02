# [M] Task sets release memory after a burst

## Goal

A long-lived task set does not keep an unjustified amount of memory for a
burst that has already drained, or the measurement shows that the retained
capacity is worth what it saves.

## Plan

[kio::Tasks](/rs/kio/src/task.rs) keeps every slot it ever grew: the slab
entry (`Option<Occupant<T>>`, whose `Park` holds an inline `Waiter` of about
100 B), the per-slot waker allocation, and the wake bitset. The CPU half of
this quest landed in [moq-dev/moq#7513](https://github.com/moq-dev/moq/pull/7513).
A three-level wake index means an owner poll visits only woken words, so poll
cost no longer depends on the historical peak. The memory half remains.

Decided 2026-11-01 by the maintainer on
[moq-dev/moq#7513](https://github.com/moq-dev/moq/pull/7513):

- ✅ (a) Land the CPU half as is, and keep this quest for the memory half:
  reclaimable slot chunks, measured against repeat-burst cost and the real
  consumers' task sizes.
- (b) Treat retained capacity as justified and delete this quest.
- (c) Implement chunk reclaim in the same PR.

The [`task_capacity` bench](/rs/kio/benches/task_capacity.rs) reports
retained bytes after a burst. With an 8-byte task, the set keeps about
184 B per peak slot whatever is still live: 188,912 B after a 1,024-task
burst and 12,085,304 B after 65,536. A real task adds `size_of::<T>()`
per slot, so measure the actual consumers (`GroupServe`, the io_uring worker's
`Task`, the session sets) before judging the cost. Dropping the set returns
the heap to baseline, so this is retained capacity, not a leak.

The candidate is reclaimable slot chunks. Store slots, wakers and wake bits per
1,024-slot chunk, and free a chunk once it empties. That needs allocation to
prefer low free slots, so chunks can drain while one survivor stays behind,
plus some hysteresis so a set oscillating across a chunk boundary does not
reallocate 1,024 wakers each time. Today's LIFO free list and full reuse make
a repeated burst cheap. In the harness from
[moq-dev/moq#7513](https://github.com/moq-dev/moq/pull/7513), a repeated
1,024-task burst through a drained set costs about 840 instructions per task,
the task's own allocations included, and the set allocates nothing for it. Do
not trade that for reclaimed memory without measuring both, with an A/A control.

Any reclamation must preserve stale-waker safety. A waker that outlives its
chunk must keep its chunk alive or point at nothing. It must also keep
concurrent wakeups, coalescing, snapshot fairness and the rotation order
(`concurrent_wakes_are_never_lost`, `wakes_past_64_chunks_run_in_slot_order`).

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify
equivalent work, schedule coverage nightly, and accept a measured no-win.
Expected public API and wire impact: none.
