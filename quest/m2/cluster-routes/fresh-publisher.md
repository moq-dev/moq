# [S] Adopt or retain a fresh publisher's doubt exemption

## Goal

Settle whether a relay moves at once onto a doubted publisher's own route,
offered on an authenticated session the publisher opened after the doubt
began, or keeps timer-only doubt, and leave the tree matching the choice.

## Plan

[moq-dev/moq#8735](https://github.com/moq-dev/moq/pull/8735) modeled and
measured the exemption beside timer-only doubt, the baseline of epic Plan
item 27. The router runs it as the comparison rule `Rule::FRESH`
(`Doubt::fresh`, test builds only), and `RouteHandoff.tla` as
`Exempt = "fresh"`. A route qualifies when its path names only the session's
authenticated peer, and that session opened after the relay's doubt of the
peer began.

- Safety: every rule holds exhaustively on `Fresh.cfg`, as on its control
  `Reconnect.cfg` (the publisher on two joined relays, three session
  changes), `NoStaleReturn` included. With a third relay behind both, the
  control and the exemption both break `NoStaleReturn` through two closes
  and no reconnect, `Twice.cfg`'s accepted case; without it, the exemption
  showed no violation in 12.9M states, every ordering to depth 14, before a
  45-minute cap. The router's fixtures and sweeps pass under the rule
  (2,000 scenarios a sweep), and churn shows no loop.
- Reconnect cases, 24 relays: the publisher's own relay recovers in one hop
  of delay instead of after the timer, but every other relay still doubts the
  publisher and refuses the forwarded route, so recovery ends 25 ms sooner in
  the clique (1,025 ms) and 75 ms sooner in the ring (1,800 ms). A publisher
  that moves to another relay gains nothing: that relay opened its session
  before the DEAD reached it. In the ring, DEADs echoing back round it extend
  each relay's doubt to as late as 1,575 ms.
- Churn (400 cases, the publisher's link flapping most): 28% fewer
  reachable but unrouted observations, but 55% more route messages, 85% more
  claims sent, and 80% more claims seen going offline. Timer-only doubt damps
  a flapping publisher; the exemption shows each flap.
- Neither broader variant breaks a rule within these bounds: `"own"`, the
  publisher's own route on a session open before the doubt, and `"session"`,
  any route on a session opened while doubting. The model's honest nodes and
  atomic closes cannot show why the exemption needs both conditions.

Adopting: move `Router::fresh` and `since` out of the test build, make the
exemption the default, mirror it in `js/net` once
[the switch](/quest/m2/cluster-routes/switch.md) runs the path router there,
and update epic Plan item 27 and the `verify-route` skill. Doubt stays local,
so the wire and the draft are unchanged.

Retaining: delete this quest. `Rule::FRESH` stays a comparison rule beside
`Rule::EARLY`, and `Exempt` in the model.

[The flapping publisher](/quest/m2/cluster-routes/flapping-publisher.md)
lets no doubt block a route at its own origin. That also frees the relays
this exemption cannot, so if it lands first, delete this quest.

## Waiting

- Maintainer: adopt the narrowly authenticated direct-publisher exemption,
  or retain timer-only doubt. Recommended: retain. Safety holds, but the
  measured reconnect cases improve by one to three hops of delay at most and
  not at all when the publisher moves, while a flapping publisher costs much
  more traffic. An agent cannot take it: adopting changes the accepted
  routing policy of epic Plan item 27. Done when this quest is deleted, or
  rewritten to the adopting work.

## Related

- [lite-07 sessions run the path router](/quest/m2/cluster-routes/switch.md) - supplies the authenticated session boundary when it puts the rule on the wire
- [A dual-homed publisher stays reachable while one link flaps](/quest/m2/cluster-routes/flapping-publisher.md) - a broader rule that would cover this exemption
