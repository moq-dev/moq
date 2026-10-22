# [M] Compute bandwidth grants without repeated whole-table work

## Goal

Rust and browser publishers receive the same fair bandwidth grants with
less repeated allocation work, when measured across real observers.

## Plan

[JS allocation](/js/net/src/bandwidth.ts) computes each reservation's grant
separately. Each call scans all wants, filters/sorts each priority tier, and
searches again for the next tier. Synchronous peek repeats it. The
[Rust sibling](/rs/moq-net/src/model/bandwidth.rs) also snapshots claims and
polls demand per share. One estimate change can duplicate work across every
observer. The survey records deterministic JS priority-read counts, including
2N² reads for all grants in one tier, before sorting.

Reproduce that operation count with the repository's Bun (not a latency test):

```bash
bun -e '
import { allocate } from "./js/net/src/bandwidth.ts";
for (const tiers of [1, 8, 64]) for (const n of [16, 64, 256]) {
  let reads = 0;
  const wants = Array.from({ length: n }, (_, id) => ({
    id, get priority() { reads++; return id % tiers; }, max: 1000,
  }));
  let sum = 0;
  for (let id = 0; id < n; id++) sum += allocate(n * 500, wants, id) ?? 0;
  console.log({ n, tiers: Math.min(tiers, n), reads, sum });
}'
```

Extend the Rust bandwidth target beyond its existing unlimited-allocator
probe-target sum; add a real-browser counterpart. Sweep connections ×
reservations, priority diversity and observer count, with demanded/idle mixes.
Measure estimate changes, demand transitions, ceiling changes and per-frame
peek separately: CPU, allocations, waker/listener registrations and browser
main-thread delay. Assert every grant, not merely their sum.

Consider calculating a shared allocation once per relevant change, but retain
strict priority, max-min fairness, rounding, ties, immediate peek behavior and
closed-demand removal. Cover both languages in this quest. Coordinate with
[viewer targets](/quest/m2/probe/viewer-target.md), which changes how the same
allocator feeds probes.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
