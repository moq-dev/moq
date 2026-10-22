# [M] Browser buffered-group bookkeeping scales with changed data

## Goal

Playing or skipping buffered groups avoids avoidable repeated backlog scans
without changing the buffering and max-delay contract.

## Plan

[Hang Consumer](/js/hang/src/container/consumer.ts) rebuilds buffered ranges
by walking groups in `#updateBuffered` (line 696). `#checkMaxDelay` scans
all groups inside a loop that shifts old groups (line 377), so bulk skipping
can repeat the scan. These paths execute while frames arrive and are read.

Add real-browser cases sweeping buffered groups × frames per group, then
players × independent publishers. Compare ordinary steady playback, sparse
out-of-order groups, large lookahead and a burst that must be skipped.
Measure operation counts, main-thread CPU, allocations/GC and frame delay.
Verify exact buffered ranges, gaps, timestamp order, skip decisions and closure
of discarded frames. Incremental extrema/ranges are candidates only if their
maintenance is cheaper across normal and adversarial arrival orders.

This extracts the recurring audit's buffered-consumer finding.
[Watch worker planning](/quest/m2/plan-watch-worker.md) owns main-thread
isolation; moving the work does not discharge this algorithmic measurement.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
