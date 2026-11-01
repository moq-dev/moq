# [XS] The moq-net memory bench's allocation mode finishes at 1 MiB frames

## Goal

`MEMORY_ALLOCS=1 cargo bench -p moq-net --features fuzz --bench memory` runs every case to completion, including `size=1048576`, and the cause of today's hang is fixed where it lives (the bench, its counting allocator, or moq-net). If the cause is in moq-net, a regression test covers it.

## Plan

Found by [moq-dev/moq#7522](https://github.com/moq-dev/moq/pull/7522): on its base [`c1a1412820`](https://github.com/moq-dev/moq/commit/c1a1412820) (`main` at the time), the allocation mode parks with no CPU after `streams=1/size=16384`, at `size=1048576`. A park with no CPU points at a lost wake or a flow-control stall, not slow work.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. Root-cause before fixing: take a stack of every thread while parked (`gdb -batch -ex 'thread apply all bt'`), check whether the counting allocator's own state is involved (compare a run without `MEMORY_ALLOCS`), and whether a 1 MiB frame exceeds a window or cache bound the bench's in-memory transport waits on. A lost wake in moq-net gets its own regression test; a bench-only cause is fixed in `rs/moq-net/benches/memory.rs`.

Public API: none. Wire: none.
