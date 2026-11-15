# [XL] A stalled route yields to its standby

## Goal

For each track with payload demand, a relay or endpoint that opted in holds
a metadata-only subscription on its next-best route to the same Instance, and
promotes it in place when the active route's delivered media falls behind
what the standby reports by more than the reader's budget. The track then
stays on the working route. A silent death and a payload stall on a live
session, which `rs/moq-net/tests/integration/route_health.rs` pins as lost
media and as never recovered, both recover within that budget.

## Plan

The [epic](/quest/m3/route-health/README.md) holds the decisions: comparison
as evidence, one standby per demanded track, the doubling exploration, no
return to a demoted route, per track, only for an Instance the publisher
named, and opt in.

- The opt-in is off by default and belongs to the node that holds the
  standby: a field in the relay's configuration and an option on the client,
  named alike in Rust, JS, and the bindings. With it off, nothing here costs
  a message or a subscription. Read the switch where a track's source is
  chosen, so a later per-broadcast switch changes one lookup.

- Each track chooses its source apart from the broadcast's shared route
  selection: the front already holds every usable route to its Instance
  (`rs/moq-net/src/model/origin.rs`, `model/front.rs`), and the router hands
  it each alternate. The maintainer permits falling back to per-broadcast
  failover if per-track proves disproportionate; say why if used.
- Promotion overlaps payload briefly while the new route is verified, and a
  steady standby carries none. Bound the overlap.
- Two relays deciding at once must not swap onto each other: the standby is
  never a route whose path names the relay, and a relay drops its standby
  when the standby is all it serves the track to.
- Model the promotion with the `verify-failover` skill before building it,
  since it adds an ordering between two subscriptions.
- Measure the interruption for a clean close, a silent death, and a payload
  stall, against today's: about 0, the 10 s idle timeout, and never.

Public API: the opt-in, in the relay's configuration and on the client,
beside [`Subscription.meta`](/quest/m3/route-health/meta.md). Wire: none
beyond it.

## Required

- [Metadata-only subscriptions](/quest/m3/route-health/meta.md) - what the standby observes
- [The switch](/quest/m2/multicast/switch.md) - the routes the standby chooses among
