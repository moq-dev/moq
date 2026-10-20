# [L] Select route copies per logical track

## Goal

A broadcast shares its identity and eligible route handles while each demanded
track can select active and standby copies independently. Existing logical
readers and same-epoch resume remain the single handoff implementation in
Rust and JS, without a route table or full broadcast front per track.

## Plan

Decided 2026-10-13: prefer per-track failover so stalled video need not move
healthy audio. Rust TrackIo already owns independent resume producers, but
Front's broadcast-wide serving/upstream source and redispatch need refactoring.
Coordinate JS with its existing logical-track handover, not another P2P or
health-specific reader. The maintainer permits broadcast-level fallback only
if the design work demonstrates disproportionate complexity and records it.

Keep candidate resolution, epoch/authorization checks and requester exclusions
shared. Select per-track copies from those candidates, with standby observations
bound to the candidate being measured. Preserve subscription aggregation and
bounded ownership when tracks, candidates, or downstream readers leave.

Normal selection still uses application priority, cost, deterministic hashing,
and newest only on otherwise identical routes. This quest provides selection
and lifetime machinery; Automatic recovery owns the detector and requalification.
Never create resumable multi-route state for an epochless broadcast.

Test independent track switches, changed/missing epoch, a disappearing candidate,
late old-copy frames, cancelled pending resolution, and candidate reuse across
many tracks. Measure tracks times routes/readers, including untouched-track
polls and retained handles. Update affected API docs and bindings if exposed.

Public API/wire: prefer private selection machinery and existing resume;
report any required surface change. No independent wire extension planned.

## Required

- [Health design](/quest/m2/route-health/design.md) - the proven selection boundary
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - shared candidate and identity model
