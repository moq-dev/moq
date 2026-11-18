# [M] What the route switch left

## Goal

No quest still describes the old route rule or the Route Key.

[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) put every
lite-07 session on `route::relay::Router`, rewrote the draft, made `js/net`
an endpoint, and deleted the old rule with its route key.
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053) made the
test-only route API crate-private, closed the adopted-claim gap with a
marked identity, marked the announce-churn mitigations, and removed
`poll::Session::export_keying_material`. What is left here is the quests
written against the old wire.

## Plan

Decided 2026-11-17 (maintainer, on
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987#issuecomment-6958210502)):
✅ this quest is kept.

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053#issuecomment-6975349236)):
✅ kept as rewritten here, retargeting the old-wire quests (rejected: dropping it).

Retarget each to the route rule that ships (`rs/moq-net/src/route/relay.rs`
and the lite-07 draft's Routing section), or delete it when the new rule
makes it moot:
[outbound federation](/quest/m3/cluster-routing/federation.md),
[the routing guide](/quest/m3/routing-guide.md),
[route wakes](/quest/m4/route-wakes.md),
[handoff routes](/quest/m4/relay-handoff-routes.md),
[regrant recheck](/quest/m4/regrant-route-recheck.md),
[announce interest memory](/quest/m4/announce-interest-memory.md),
[the mesh condition](/quest/m5/lite07-mesh.md),
[route memory](/quest/m9/perf/cluster-route-memory.md),
[route fan-out](/quest/m2/cluster-routes/route-fanout.md), and
[the Playwright bump](/quest/m1/playwright-pin.md), whose only reason was the
route key lane.

Each retargeted quest also drops its link to this one.

Public API: none. Wire: none.

## Related

- [Cache-aware multicast](/quest/m2/multicast/README.md) - the epic
- [Route benches](/quest/m3/route-benches.md) - the path-rule benches and costs the switch left
- [Proof binding](/quest/m3/route-proof-binding.md) - binds the route proof to the router
- [Claim chain](/quest/m2/multicast/claim-chain.md) - the chain change the cut waits on
- [The moq-cluster draft](/quest/m2/moq-cluster-draft.md) - kept, since third-party relays ship it
