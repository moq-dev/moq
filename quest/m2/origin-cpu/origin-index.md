# [M] Measure and reduce origin route-selection CPU

## Goal

Reduce demonstrated route-selection overhead while preserving source
identity, ranking, exclusion, failover, and announcement behavior.

## Plan

The current `rs/moq-net/src/model/origin.rs` stores alternative sources in
`FrontState::routes` per broadcast. `FrontState::best_route` selects from
that set; it does not scan a global advertisement table. Trace current
lookup, source-update, and announcement paths before choosing an index.

Add a registered Criterion target using public origin operations. Sweep
broadcast count, sources per broadcast, cursor count, and update churn
independently. Separate initial replay from incremental notification and
exact-path lookup; replay must pay for every result it emits.

Validate alternate-source wins with a relay workload where the same content
and publisher identity reaches each broadcast over multiple peer routes.
Assert the observed source count, then exercise route changes, withdrawal,
and failover while checking delivered output. Independent publishers reusing
a path are not equivalent: content replacement must not masquerade as multiple
routes to one publisher. Add this workload to the existing benchmark runner.
Keep `rs/moq-bench/config/announce.toml` as a single-source regression control;
its unique per-connection paths do not validate alternate-source scaling.

Optimize only a measured bottleneck. Compare a retained linear scan at small
source counts with any proposed cache or index, accounting for invalidation,
extra memory, and update cost. Preserve the complete current `route_order`
and exclusion rules. No replacement matcher, route semantics, public API,
or wire change belongs here.

Acceptance includes paired CPU/allocation results and CI regressions for
source replacement and removal, route changes, exclusion changes, failover, and scoped
announcement delivery. A measured no-win closes the quest with retained
evidence. Do not claim sublinear replay when the result set grows linearly.

## Related

- [Relay memory](/quest/m2/relay-memory.md) - memory measurements
- [Origin local tree](/quest/m2/origin-cpu/origin-tree.md) - path traversal costs
- [Path patterns](/quest/m2/path-patterns/README.md) - owns future matching semantics
- [Wildcard advertisements](/quest/m2/wildcard/README.md) - owns future route resolution
