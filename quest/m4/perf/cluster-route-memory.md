# [M] Full-mesh cluster routes cost less memory, and the hop's group cost is profiled

## Goal

Two follow-ups of the kio channel-contention spikes, each measured:

- The announce state a relay holds per route per broadcast in a full-mesh cluster shrinks from about 3.7 KB, so a mesh of W peers stops paying W squared times that for every broadcast, or the quest records why that state is needed.
- The in-memory moq-lite hop's cost of about 140 thousand user instructions for a one-frame group is profiled (`cpu`), with the avoidable parts fixed or handed to the quest that owns them.

## Plan

From [moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534) (the relay-per-thread cluster spike) and [moq-dev/moq#7533](https://github.com/moq-dev/moq/pull/7533) (the replica spike).

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): in the perf epic, one quest for the spikes' measurement follow-ups.

Moved to lab 2026-11-12 (maintainer, /quest-audit): a measurement with no public API or wire change.

- **Route memory.** #7534 meshed W origins over moq-lite-06 `memory` sessions, as dialed cluster peers do. Those numbers predate the lite-07 route layer (`rs/moq-net/src/route/`), so measure again on lite-07 before breaking anything down. Announce heap per broadcast went from 4.9 KB with one origin to 45 KB at 4 workers and 886 KB at 16 (240 routes, about 3.7 KB each), and allocations from 64 to 14,600; 10,000 broadcasts at 16 would hold about 9 GB. Real full-mesh relay clusters pay the same per route. Measure it on a relay cluster with the counting allocator (#7534's `rs/moq-relay/examples/spike_announce.rs` is in its prototype commit [`ddd8b04`](https://github.com/moq-dev/moq/commit/ddd8b042ab82c7a4d53957e31fe35942957b7dfb), not on `main`), break the 3.7 KB down by structure (origin route entry, per-session announce state, hop lists on lite-06, cursors), and shrink what's duplicated per route. On lite-07 a route leads to a relay rather than a publisher ([moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987)), so the per-broadcast state to break down is each claim's copy per neighbour in `route::relay::Router`, its slot per claimant and session in `route::node`, and its origin route entry, with no hop list.
- **Hop cost.** About 140 thousand user instructions per one-frame group across one in-memory hop; fanout's 64 subscribers spread over 15 hops raised user instructions 19%, while chat amortized it. Take a `cpu` profile of one hop and attribute it before fixing; a per-reader part belongs to [mesh session cost](/quest/m4/perf/mesh-session-cost.md), which owns the per-reader group cost.
- **`bench-profile lock`**: moved to [its own quest](/quest/m4/perf/profile-lock-switches.md) on 2026-11-03 (maintainer, /quest-plan of the follow-ups from [moq-dev/moq#7742](https://github.com/moq-dev/moq/pull/7742)), which keeps refusing nonalternating switch records and finds their cause instead of tolerating them.

Public API: none expected. Wire: none.

## Related

- [kio channel contention](/quest/m5/kio-channel-contention/README.md) - the epic both spikes reported to
- [Cap the legacy announce interest streams](/quest/m4/announce-interest-memory.md) - the same per-stream announce state, from a subscriber's side
