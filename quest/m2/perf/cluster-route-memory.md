# [M] Full-mesh cluster routes cost less memory, and the hop's group cost is profiled

## Goal

Three follow-ups of the kio channel-contention spikes, each measured:

- The announce state a relay holds per route per broadcast in a full-mesh cluster shrinks from about 3.7 KB, so a mesh of W peers stops paying W squared times that for every broadcast, or the quest records why that state is needed.
- The in-memory moq-lite hop's cost of about 140 thousand user instructions for a one-frame group is profiled (`cpu`), with the avoidable parts fixed or handed to the quest that owns them.
- `just bench-profile lock` tolerates a few lost perf records and nonalternating switch records, reporting how many it lost, instead of failing the run.

## Plan

From [moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534) (the relay-per-thread cluster spike) and [moq-dev/moq#7533](https://github.com/moq-dev/moq/pull/7533) (the replica spike).

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, in the perf epic, one quest for the spikes' three measurement follow-ups.

- **Route memory.** #7534 meshed W origins over moq-lite-06 `memory` sessions, as dialed cluster peers do. Announce heap per broadcast went from 4.9 KB with one origin to 45 KB at 4 workers and 886 KB at 16 (240 routes, about 3.7 KB each), and allocations from 64 to 14,600; 10,000 broadcasts at 16 would hold about 9 GB. Real full-mesh relay clusters pay the same per route. Measure it on a relay cluster with the counting allocator (#7534's `rs/moq-relay/examples/spike_announce.rs` is in its prototype commit [`ddd8b04`](https://github.com/moq-dev/moq/commit/ddd8b042ab82c7a4d53957e31fe35942957b7dfb), not on `main`), break the 3.7 KB down by structure (origin route entry, per-session announce state, hop lists, cursors), and shrink what's duplicated per route. Check against [cluster routing](/quest/m1/cluster-routing/README.md)'s per-origin routes first, since they change what a route holds.
- **Hop cost.** About 140 thousand user instructions per one-frame group across one in-memory hop; fanout's 64 subscribers spread over 15 hops raised user instructions 19%, while chat amortized it. Take a `cpu` profile of one hop and attribute it before fixing; a per-reader part belongs to [mesh session cost](/quest/m2/perf/mesh-session-cost.md), which owns the per-reader group cost.
- **`bench-profile lock`** (`bench/profile.py`): both spikes lost runs to it. #7533 could not record base chat at 16 workers on that host (every attempt lost a perf record or was refused for nonalternating switch records), and #7534 raised the perf buffer by hand (`SPIKE_PERF_BUFFER`). Raise the buffer for high-switch shapes, skip or pair up nonalternating switch records, and report the lost-record count beside the result so a reader can judge it. Cover it in `bench/profile.test.py`.

Public API: none expected. Wire: none.

## Related

- [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md) - the epic both spikes reported to
- [Announce interest streams share one route table](/quest/m2/announce-interest-memory.md) - the same per-stream announce state, from a subscriber's side
