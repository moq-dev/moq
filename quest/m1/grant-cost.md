# [S] A grant prices its session

## Goal

`moq_auth::Grant` carries an optional `cost`: the price the relay holding
the session adds to every route the session's peer announces, as
`Client::with_cost` does for a dialed link. `Lease::update`
(`rs/moq-auth/src/lease.rs`) reprices a live lease in place, so the relay
re-ranks the session's routes without a reconnect. A grant without a
`cost` leaves the link priced as today.

## Plan

Decided 2026-11-25 (maintainer, /quest-plan via kixel-ai/kixel.ai):

- ✅ Repricing is local, with no new MoQ wire message (rejected: a wire
  update pushing a session's cost to the peer, which needs versioning and
  has no consumer). Other relays learn the new cost from the route adverts
  they already get.
- ✅ m1, since it is additive.

Kixel's mesh coordinator is the consumer (its `quest/m3/link-quality.md`
and `quest/m3/p2p/cost-scopes.md`). It prices each link from measured quality and
reprices it as quality moves.

Guidance:

- A grant's cost wins over the cost the peer declares in SETUP, the way a
  price the dialer sets does. Zero is refused at validation, since
  moq-lite-07 routing needs every link to cost something.
- The relay's route port takes its cost once, at connect
  (`relay::Port` in `rs/moq-net/src/lite/subscriber/routed.rs`). Repricing
  needs a way to change it on a live port, so the routes behind it re-rank
  and are re-advertised with the new cost.
- The auth endpoint's answer gains the field. A relay that predates it
  refuses the grant, since `Grant` refuses unknown fields. That's
  fail-closed, but an auth server must send `cost` only to relays that read
  it (`Request::relay`), and the PR's Impact section says so. A token claim
  for it waits for a consumer.
- Mirror the field in `js/auth`'s `GrantSchema` (`js/auth/src/contract.ts`)
  and its corpus. `@moq/net`'s `Grant` is the peer's `AUTH_OK` answer, which
  this leaves alone.

## Related

- [Dial and listen grants](/quest/m3/proxy/grants.md) - adds fields to the same `Grant`; whichever lands second rebases onto the other
- [JS session cost](/quest/m1/js-session-cost.md) - pricing a link from the dialing side
- [Cluster routing](/quest/m3/cluster-routing/README.md) - how relays re-advertise a repriced route
