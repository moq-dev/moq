# [S] Decide whether lite-07 freezes with warm routes

## Goal

Before lite-07 freezes, the maintainer settles whether WARM_START,
WARM_UPDATE, WARM_END, and SUBSCRIBE's `Warm` byte ship in lite-07, and the
draft, `rs/moq-net`, and `js/net` match that decision. Until a production
implementation exists, the merge gate stops paying for the cache sweep.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md)
on [moq-dev/moq#8732](https://github.com/moq-dev/moq/pull/8732).

- The rule lives only in test code (`rs/moq-net/src/route/tests/cache.rs`,
  `offer_from` and `decide`); production reads the warm messages and ignores
  them. The sweep's `steady()` and `fixed_point()` check relays against the
  same functions they run, so it is a consistency check, not an independent
  oracle, and it checks none of the Goal's efficiency properties: a rule that
  never offered a copy would pass it.
- `sweep_cache_routing` was the slowest route test, 36 s in a debug run, on
  every moq-net PR, for code that never ships; the long sweep already runs
  nightly from `test/route-sweep.sh`.
- No sweep compares copies or relays crossed against plain path routing,
  which already shares one copy along each publisher's tree; the only gain
  shown is LAN siblings with their own uplinks
  (`lan_relays_share_uplink_copies`). Pull cycles form in 3.3% of churn steps
  and about 9 in 10 of those leave a reader unfed (#8732), a stall plain
  routing cannot have.
- The warm wire changed on 2026-11-12, 11-13, and 11-14, while the
  implementation ([select](/quest/m3/cache-routing/select.md), XL) lands
  after the cut. Item 7 of the [epic](/quest/m3/cache-routing/README.md)
  moved the proof into m2 for a downstream fleet plan, and on 2026-11-14 the
  mesh work moved downstream
  ([moq-dev/moq#8756](https://github.com/moq-dev/moq/pull/8756)); item 11's
  choice of lite-07 had neither as evidence.

Recommended: (b), and before select is planned, measure copies and relays
crossed against plain path routing on an edge/core topology; if the only win
is LAN siblings, (d). Whatever is picked, move `sweep_cache_routing` out of
the gate into the nightly sweep until a production rule exists.

Public API: none. Wire: lite-07-wip loses or keeps the warm messages and the
`Warm` byte.

## Waiting

- Maintainer: (a) keep the warm wire in lite-07 as it is; (b) strip it from lite-07 and re-add it in the then-current wip version with the implementation and a sweep that drives production code (recommended); (c) keep the WARM_* messages, which receivers skip by length, but drop the `Warm` byte; (d) move the cache-routing epic downstream with the mesh work
