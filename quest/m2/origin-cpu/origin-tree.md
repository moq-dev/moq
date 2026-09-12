# [M] Measure and reduce origin path-traversal overhead

## Goal

Reduce measured exact-path create/resolve costs while preserving scoped
views, notifications, source reservations, and prune-on-empty behavior.

## Plan

`OriginNode` contains `HashMap<String, Lock<OriginNode>>` children.
Traversal locks nodes by segment and insertion owns each segment string.
`PathOwned` can share `Arc<str>` storage; shared storage does not imply
interning or constant-time hashing.

Benchmark public create/resolve operations with 10k through 100k paths,
depths 2 through 8, shared and disjoint prefixes, and concurrent churn.
Measure lock contention, allocations, memory, and tail latency. Account for
bytes hashed rather than promising cost independent of path length.

Compare the current tree with a simpler locking or lookup arrangement only
if the baseline identifies a useful win. Preserve source reservations,
stale-owner identity checks, announcement registration/replay, scoped roots,
and removal during replacement. A global lock must not improve a serial
microbenchmark at the expense of concurrent publishers.

Register a Criterion target independently if the route-selection quest has
not landed. Wire lifecycle and scope regressions into normal CI and retain
paired results. Keep the current tree if alternatives do not improve the
measured workload. Coordinate with pattern scopes so storage changes retain
their literal-head notification and filtering requirements.

## Related

- [Route-selection CPU](/quest/m2/origin-cpu/origin-index.md) - per-broadcast alternatives
- [Relay memory](/quest/m2/relay-memory.md) - memory measurements
- [Origin scopes](/quest/m2/path-patterns/origin.md) - future pattern-scoped views
