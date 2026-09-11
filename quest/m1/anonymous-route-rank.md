# [M] moq-net: an anonymous route never outranks an identified one

## Goal

A route that passes through an anonymous hop, at any depth, ranks below every
route whose hops are all identified, whatever the costs say. If Cloudflare's
relay announces broadcast A without an identity at cost 1 and a cost-5 route
through identified relays reaches the real publisher, the cost-5 route wins,
on this relay and on every relay downstream of it. Among anonymous routes,
cost keeps ordering as it does today. On dev, where the route model lives.

## Plan

Today `route_order` in `rs/moq-net/src/model/origin.rs` ranks on
`(Cost, len, hash, id)` and nothing about identity enters it. An anonymous
peer's announce is stamped as a one-hop route charged the link cost
(`session_route` in `rs/moq-net/src/ietf/subscriber.rs`, default 1), so it
beats any identified route costing more, and the `cold` half of
`Cost::UNKNOWN` never fires because cold only breaks a warm tie. The doc
comment on `session_route` already names the hazard and points at
`Client::with_cost` as the workaround.

Hop 0 in a chain is the mark, and it travels:

- A relay bridging an anonymous upstream writes 0 for that hop on the wire,
  in lite-06 HOP_PATH and the cluster extension alike, and forwards a
  received 0 unchanged. Its minted per-session id stays local: `RouteEntry`
  gains `via: Hop`, the announcing session's declared or assigned id, and the
  split-horizon filter in `best_route` and the advertise path matches `via`
  as well as the chain, so #3042's echo fix (a route is never advertised back
  to the session it came from) holds without the id ever leaving the relay.
  This reverses the direction of #3060, whose quest is deleted with this one:
  an assigned identity is private selection state and forwarding it published
  a name for a peer that declined to give one.
- `route_order` becomes `(anonymous, Cost, len, hash, Reverse(id))`, where
  `anonymous` is whether the chain holds a 0 anywhere; the rest of the key is
  dev's as it stands. Main's leading `!announce` term and its `attach_source`
  takeover gate do not exist on dev: every route in the table is announced,
  routes for one prefix coexist, and `best_route` is the whole decision, so
  arrival order cannot let an anonymous route replace an identified one.
  Lite-03 hop-count placeholders are 0 entries and count as anonymous, which
  is what they are. `Route` exposes `is_anonymous()` for the bindings and the
  announcement stream.
- `js/net` keeps accepting 0 inside a received chain and exposes `anonymous`
  on the announcement it yields; it ranks nothing today, and the browser hop
  in [P2P](/quest/m3/p2p/README.md) forwards 0 like a relay when it lands.
  `doc/concept/transport.md` states the selection rule beside route cost.
- Loop detection is unchanged: a 0 entry matches nothing, and a relay's own
  id still appears in the chain wherever it forwarded the route.
- Drafts: `drafts/draft-lcurley-moq-cluster.md` "Assigned Identities" says
  the assigned id is local selection state that MUST NOT be forwarded, and
  "Bridging" writes 0 for an upstream that sent no HOP_PATH; the selection
  rule prefers a HOP_PATH with no 0 entry before comparing ROUTE_COST.
  `drafts/draft-lcurley-moq-lite.md` mirrors both for lite-06. Validate with
  `just drafts check`.
- Tests: an anonymous cost-1 route loses to an identified cost-5 route; two
  anonymous routes order by cost; a chain `[0, R1]` received from identified
  peer R1 loses to `[P, R2]` at a higher cost; a relay forwards `[0, R1]` with
  the 0 intact and never advertises the route back to the session it came
  from; `anonymous_routes_never_resume` and the #3042 filter tests keep
  passing. Run `just test smoke-full`, since the interop relays are anonymous.

## Related

- [Rank](/quest/m2/pop-skipping/rank.md) - warm-copy adoption, which also
  refuses to treat two anonymous relays as one
- [Route cold cost](/quest/m1/route-cold-cost.md) - the cost pair the bindings
  carry, which `is_anonymous()` joins
- [#3060](https://github.com/moq-dev/moq/issues/3060) - the ban on hop 0 in
  chains this quest decides against
