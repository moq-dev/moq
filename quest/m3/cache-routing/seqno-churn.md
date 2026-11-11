# [M] Origins advance their seqno only when a neighbor needs it

## Goal

An origin's seqno advances only when some node needs a newer route to it,
so a warm child stops moving off its warm parent and back each time the
origin opens or closes an announce stream. Recovery after an origin
reconnects or restarts under the same Node ID stays as fast as it is today.

## Plan

Found by the cache-routing proof
([moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457), the epic's
item 12): an origin advances its seqno on every announce stream open and
close, and an advance that reaches a child before its warm parent makes the
parent's older route ineligible, so the child moves off and back (2 moves in
the chain fixture of `rs/moq-net/src/route/tests/cache.rs`).

The advance on every session change exists so feasibility distances held
elsewhere accept a restart reusing an identity and an origin that moved to a
farther attachment (`restart_reusing_identity_is_refused`,
`reconnect_without_restart_needs_a_seqno_advance`, and
[Routes and announces](/quest/m2/cluster-routing/routes/README.md)).
Recommendation: advance only on a ROUTE_REQUEST, as Babel does, plus once
per restart, and measure what the extra round trip costs those two tests'
recoveries. Update the draft's Routing section, which says an origin
advances whenever its own set of sessions changes, in the same PR.

## Waiting

- Maintainer: whether one ROUTE_REQUEST round trip on a reconnect to a
  farther attachment is an acceptable price for fewer warm moves. An agent
  can measure both but not weigh them. Recommendation: yes, since the
  request path already exists and an idle origin then sends no advances.

## Related

- [Warm routes and per-broadcast parent selection](/quest/m3/cache-routing/select.md) - the warm children whose moves this cuts
