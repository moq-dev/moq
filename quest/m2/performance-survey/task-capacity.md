# [M] Task polling recovers after a burst

## Goal

Long-lived task sets do not pay an unjustified steady-state CPU or memory
cost for a burst that has already drained.

## Plan

[kio::Tasks](/rs/kio/src/task.rs) grows slots and per-slot wakers, retains
them after completion, and snapshots every allocated wake-bit word in each
`Tasks::poll`. Dispatch scans those words again. Work can depend on
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

A 2026-10-22 staging CDN profile (`f57c0aad`, DWARF stacks, 27-36% incomplete)
put `Tasks<GroupServe>::poll` at 2.40% self-time on a 501-connection chat
shape and 1.90% on SD, the largest moq-net site on both, and kio
tasks/waiters at 7.5-11.5% of samples. Self-time only; it does not show how
much is historical capacity.

Related: [worker quiescence](/quest/m2/perf/uring-quiescence.md) and
[relay allocations](/quest/m2/perf/relay-allocs.md) own other costs.
Worker quiescence also reshapes the `Tasks::poll` snapshot with a pass
budget; whichever of the two lands second rebases its benchmark on the other.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
