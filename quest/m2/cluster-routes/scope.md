# [M] Settle the path router's doubt lifetime

## Goal

Choose and verify the doubt policy for `route::path::Router` before
[the switch](/quest/m2/cluster-routes/switch.md) puts it on the wire.
[moq-dev/moq#8622](https://github.com/moq-dev/moq/pull/8622) implements the
sans-IO router with a route per scope, beside the distance router, and
compares both doubt policies. Its remaining policy decision is below.

## Plan

The implementation preserves the model's early-collection default:
`Lift::Unnamed` lifts doubt once no heard route names the node.
`Lift::Timer` keeps it until the one-second timer fires. Both are measured
on identical scripts. The PR records the complete measurements and validation.

The model's `Loop.cfg` records a request loop with early collection while
news of three closed sessions is still in flight. Prior bounded checks
keeping doubt until the timer avoided that counterexample; this is not a
proof that arbitrary timer firings prevent all request loops.

The 120,000-step churn comparison has 16,536 stale-route observations and
56,663 reachable-but-unrouted observations with early collection, versus
3,312 and 162,654 with timer-only doubt. Observations count relays after
each step, not time or distinct outages. The fixed reconnect cases wait
under either policy because heard routes still name the doubted node:
on 24 relays, rejoining takes 1,050 ms in a clique and 1,875 ms in a ring,
including propagation at 25 ms per hop. Today's distance rule takes 50 ms
and 325 ms there.

After the maintainer chooses, implement the default and mirror it in
`RouteHandoff.tla`. If retaining doubt changes the modeled rule, follow
the original plan: `Loop.cfg` must pass and `RequestsLoopFree` joins the
invariants every config checks. Reproduce any new counterexample on the
router instead of weakening an invariant. Update the verify-route coverage,
rerun the affected checks and local review, then delete this quest and its links.

## Waiting

- Maintainer: choose (1) keep doubt until its one-second timer fires
  (recommended: avoids the known early-collection counterexample and
  reduces stale routes), or (2) collect it once no heard route names the
  node (fewer reachable-but-unrouted observations during churn, with the
  known loop). Benchmarks and model checks supply the facts; an agent
  cannot supply the maintainer's previously reserved policy choice.

## Related

- [lite-07 sessions run the path router](/quest/m2/cluster-routes/switch.md) - puts the chosen rule on the wire
