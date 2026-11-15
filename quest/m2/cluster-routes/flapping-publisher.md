# [S] A dual-homed publisher stays reachable on real sessions while one link flaps

## Goal

On real lite-07 sessions, a publisher connected to two relays whose link to
one of them closes and reopens faster than the doubt timer stays reachable
through its other link, while the flapping link waits out the timer each time
it returns. A relay that already hears a route through the other link that
does not name it stays routed the whole time; any other relay is routed again
once a neighbour re-offers, a round trip after the news, instead of after the
timer. The draft states the rule.

## Plan

[moq-dev/moq#8734](https://github.com/moq-dev/moq/pull/8734) settled the rule
([epic](/quest/m2/cluster-routes/README.md) Plan item 27, decided 2026-11-14):
doubt blocks a route through a node, not one that begins at it, except the
node's own route on a session to it opened since the doubt began. The model
checks it (`Flap.cfg`, `TwoHomes.cfg`, `Homes.cfg`, and `Routed`), the path
router applies it, and `route/tests/flap.rs` pins it on the router.
[The switch](https://github.com/moq-dev/moq/pull/8731) put lite-07 sessions on
the path router, so what is left is:

- A fixture on real sessions with mocked time: the dual-homed publisher with
  one link flapping at half the timer, where a subscriber at a far relay
  receives every group while it flaps.
- The draft's Routing section: doubt keeps a relay off a route through the
  far end of a closed session, and off that node's own route on a session to
  it opened since, but not off the route it already uses or another route that
  begins there; a publisher on three or more relays that leaves them all at
  once can loop a request until the news arrives.

## Related

- [A request loop heals once routes settle](/quest/m3/cluster-routing/request-loop.md) - heals the loop a publisher leaving three relays at once can cause
