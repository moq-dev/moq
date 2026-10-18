# [S] An unroutable request waits on the front, not in a retry loop

## Goal

`origin::Consumer::routed_broadcast` retries: it registers a watch on the
path, asks, and on `Unroutable` waits for the covering routes to move. The
wait lives beside the fronts the driver runs rather than in them. The watch
is path-scoped: unrelated table changes do not re-ask the path, and a path
without coverage fails before minting a front. A covering handler that
refuses with `Unroutable`, however, ends its front; each subsequent covering
change re-asks the path and mints another front. A request for an unroutable path should be
able to mint a front that waits for coverage, and the loop should go away.

## Plan

The obstacle, and the reason #3901 rejected this as an alternative, is that
fronts are shared per path. A front that parks would park every requester at
that path, including `request_broadcast`, whose whole contract is a verdict
now. So the disposition has to travel with the requester rather than the
front: `request_broadcast` resolves the current verdict at once without
parking, while `routed_broadcast` stays registered until its route completes.
The front ends only once every parked requester has been handled, so one
requester's disposition never ends another's wait.

Decided 2026-10-08: benchmark first. Build the parked front only if the
re-mint shows as a real slope; the same benchmark then shows the replacement
is cheaper. A measured no-win deletes this quest.

Pending: whether to park is undecided until the benchmark has been timed. No
timed results exist yet; the next step is the quiet-host measurement below,
then either the parked front or deleting this quest.

The `origin/viewer_*` benches do not cover the retry loop.
`origin/unroutable_churn` sweeps 0, 1, 32, and 256
requesters against 1 and 32 repricings after a covering handler refuses.
Shared and distinct paths separate requester fan-out from front count;
unrelated repricings and zero requesters provide controls. Each change is
driven through refusal and back to waiting, without sleeps or spawned tasks,
polling only the requests a change woke so harness polling does not scale with
requesters; unrelated repricings assert they wake none.
It runs under the existing Rust benchmark nightly.

Measure repeated A/A runs on a quiet host before choosing whether to park.
The benchmark asserts each distinct path still reaches its handler after a
covering change, so it can also measure parking without removing those
legitimate attempts. Record host load, CPU affinity, compiler/profile, and
per-change cost alongside the control cases.

A parked front must not retire: a front nobody holds and nobody reads ends,
but only once it serves from a source with no route request pending
(`Front::idle` in `rs/moq-net/src/model/front.rs`).

Public API: no signature change expected. `routed_broadcast` and
`request_broadcast` keep their contracts; only where the waiting happens
changes.

Fronts are keyed by effective exclusion (`Horizon::effective`), so viewers
share them.
