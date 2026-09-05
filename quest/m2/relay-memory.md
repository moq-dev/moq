# [S] Relay memory per announcement

## Goal

A relay's memory scales with what it serves, not with what the mesh knows,
and the numbers behind that claim are current. An operator can also see how
many routes a relay holds for a path.

## Plan

Every published figure predates two changes and is untrustworthy:

- [moq#2989](https://github.com/moq-dev/moq/pull/2989) cut `kio`'s inline
  waiter slots from 32 to 4, so a `kio::State<()>` went from 896 B to about
  200 B. Going lower trades memory for an allocation per wake, which
  `kio`'s `tests/waiter_allocs.rs` pins; that lever is spent.
- [moq#3225](https://github.com/moq-dev/moq/pull/3225) made a received
  announcement a `RouteEntry` plus one `ServeState` whose cache materializes
  a broadcast only when something requests a path. A standby route is a
  table entry, not an object graph, so degree stopped being a multiplier.

The old baseline was 8.8 KB per announced broadcast plus 4.3 KB per extra
route on `adad52b`, measured with two throwaway `moq-net` examples driving an
origin under a counting allocator and reading `/proc/self/statm`. Neither is
committed, since they need `#[doc(hidden)]` size probes on private types.
Rebuild them from this description and restate the per-broadcast and
per-route cost, the per-peer session bookkeeping (`announce_ids`, `held`,
`watched`), and the shed threshold on a degree-5, 1 GB node, before anyone
quotes a number again. Two committed directions depend on the answer:
chat-shaped traffic (one broadcast per channel or per chatter) and
[PoP skipping](/quest/m2/pop-skipping/README.md), which triples average
degree and adds a second, more specific route per carried broadcast.

Then expose the count of routes covering a path from `origin`, next to what
`best_server` already walks, and surface it wherever `moq-relay` reports node
state. A count, not an iterator: the caller is a gauge, and a routes iterator
is a much larger surface to keep stable.

## Related

- [Group charge](/quest/m0/group-charge.md) - the group cache charges what a cached group really costs; a group holds one of the same state cells
- [Perf](/quest/m1/perf/README.md) - the hot-path work that owns the remaining per-cell cost
