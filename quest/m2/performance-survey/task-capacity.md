# [M] Task polling recovers after a burst

## Goal

Long-lived task sets do not pay an unjustified steady-state CPU or memory
cost for a burst that has already drained.

## Plan

[kio::Tasks](/rs/kio/src/task.rs) grows slots and per-slot wakers, retains
them after completion, and snapshots every allocated wake-bit word each poll
(lines 270-298, 364-370). Dispatch scans those words again. Work can depend on
historical peak/64 even when only one task remains. The io_uring worker and
session serving loops are current consumers.

Extend [task benchmarks](/rs/kio/benches/tasks.rs) with peak slots × current
live/ready slots: burst then one survivor, sparse live slots, full occupancy,
and empty-but-polled. Report instructions and CPU per owner poll/per ready
task, allocations and retained bytes after draining. Compare a long-lived
worker or session before and after connection/subscription churn.

The present sparse-wake case fixes 16 ready tasks among 1,000 residents; it
does not isolate historical capacity. Any reclamation or ready-word index
must preserve stale-waker safety, concurrent wakeups, coalescing and snapshot
fairness. Do not trade cheap burst reuse for churn without measuring both.

Related: [worker quiescence](/quest/m2/perf/uring-quiescence.md) and
[relay allocations](/quest/m2/perf/relay-allocs.md) own other costs.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
