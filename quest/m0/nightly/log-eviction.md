# [M] Log eviction passes the ARM concurrency model

## Goal

The log eviction model passes on ARM and x86, with every segment access protected from reclamation.

## Plan

[The 2026-10-28 Loom job](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001821) aborts in
`a_log_reader_racing_eviction_never_touches_freed_memory` with a concurrent
read/write of `UnsafeCell`. The original panic precedes the destructor panics.

The isolated model passes on x86 (90.83 s). A scratch reproduction using the
same model and forcing crossbeam-epoch 0.9.21's non-x86 `pin` branch fails in
`kio::log::Segment::free`. The x86 branch uses a sequentially consistent CAS;
the non-x86 branch uses a relaxed store and a sequentially consistent fence.
This reproduces the failing path, not a proven production use-after-free.

Determine whether the log's epoch/hint protocol or its model integration is
wrong, reproduce the interleaving, and fix the cause. Preserve reclamation of
evicted entries while readers are idle and protect borrowed entries while a
read is pinned. The x86 pass alone cannot validate the fix.

Public API: none expected. Wire: none expected.
