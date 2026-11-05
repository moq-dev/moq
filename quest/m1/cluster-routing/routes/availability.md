# [M] Announcement availability

## Goal

An announcement carries availability, and letting go is a distinct offline
event, separate from the origin's END, in Rust and in `js/net`'s
announcement API (decision 3 of the epic's handoff proof). A held route
shows its claims unavailable; a let-go shows them offline; only the origin
ends them. Bindings mirror Rust.

## Plan

- Decided 2026-10-16: an announcement carries availability, and letting go
  is a distinct offline event, separate from the origin's END. Rejected:
  presence alone, which gives a reader nothing to re-request on when a held
  route recovers; let-go reported as END, as today; and availability with
  let-go kept as END, which hides whether the origin ended or the network
  lost it.
- Build on [Listing and follow](/quest/m1/api-audit/route/events.md)'s
  listing and its offline report rather than adding event kinds beside it.
- JS gets the API shape here, fed by what `js/net` knows today; the
  [js/net route layer](/quest/m1/cluster-routing/routes/js.md) feeds it from the wire.

## Required

- [Route session integration](/quest/m1/cluster-routing/routes/session.md) - the router whose held and let-go states this reports

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the handoff contract's held and let-go states
