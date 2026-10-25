# [M] Prove active/passive delivery health

## Goal

A reviewed design and controlled-time fixtures define metadata-only progress,
local stall evidence, promotion, and recovery for epoch-bound tracks at both
endpoints and relays. Update the implementation quests with measured choices;
do not add speculative production code or choose arbitrary timing thresholds.

## Plan

The [line's decisions](/quest/m2/route-health/README.md) are settled. Start by
reproducing a stall that transport failure and ordinary reconnect miss; if
none exists, bring abandoning the line back to the maintainer. A killed relay
in a same-epoch diamond qualifies until the idle timeout fires
([#6062](https://github.com/moq-dev/moq/issues/6062)); measure the media it
loses and how far a standby shortens that, starting from
[#6183](https://github.com/moq-dev/moq/pull/6183)'s mock-time diamond in
`rs/moq-net/tests/integration/route_change.rs`. Preserve
application priorities, automatic health-driven switching and failback, a
payload-free steady standby, and bounded payload overlap to verify delivery.
The mechanism is entirely excluded for epochless broadcasts.

Keep three facts separate: what the source reports, what payload the local
reader can actually use, and what that reader is demanding. Existing
`largest()` planning includes remote hints, so comparing two largest values
alone can miss stalled media with healthy control traffic. Group/frame position
also does not prove completion of an earlier frame or availability of a gap.

Work through `Subscription.meta: true` over ordinary group/frame framing.
Define when progress is emitted, frame completion, legitimately empty frames,
mode updates racing group streams, and promotion resuming at a missing byte or
frame rather than skipping payload represented only by metadata. Reconcile
with planned ranges and SUBSCRIBE_DROP. Metadata must never satisfy a media
read, populate a false payload cache, or advance a media resume cursor.

Prove end-to-end demand aggregation: metadata-only readers cause no upstream
payload pull, mixed readers retain the payload demand they actually need, and
metadata observers do not recursively create alternate monitoring branches.
Standby observations must remain tied to the candidate being evaluated rather
than silently following the active route through ordinary aggregation. Different
first hops do not imply independent paths; include common upstreams and a
candidate returning through the requester.

Prove bounded exploration beyond the initial next-best standby when active
and standby share a failure. One standby is a concurrency bound, not a fixed
choice: define how outstanding demand and insufficient evidence advance through
other eligible candidates without treating silence as failure, opening every
alternative, or polling quiet tracks forever. Test a healthy third route behind
two stalled candidates, all candidates quiet, and candidate churn; measure
observation cost and time to discover useful progress. Settle scheduling and
budgets from these fixtures before implementation.

Test quiet publishers, empty groups, partial frames, out-of-order delivery,
receiver backpressure, metadata advancing while payload stalls, cached/stale
reports, epoch changes, and simultaneous failure/recovery at several hops.
Measure when evidence is sufficient to try a candidate, prove payload before
retiring usable old copies, and prevent immediate bounce back to a preferred
but still broken route. Recovery checks must be demand-driven and bounded;
reusing a fixed idle PROBE loop would recreate #5382.

Per-track selection is the target. Rust TrackIo/resume already splices a
single track, while Front still chooses a broadcast-wide source. Share identity
and candidate handles; do not allocate a full route table/front per track.
If the authorized broadcast-level fallback is needed, record why and update
the affected tests and public behavior consistently.

Exact framing and thresholds follow this evidence. Bring consequential API or
wire choices back to quest-plan before implementation. Publish synthetic
fixtures and retained-state/control-work measurements in existing CI lanes.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - candidate identity, identity-named requests, and transit priority semantics
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - mode changes must preserve requested positions and demand
