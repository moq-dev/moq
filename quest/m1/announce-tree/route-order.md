# [M] Rendezvous ranking

## Goal

Every relay ranks competing routes for a prefix the same way. Cost and hop
count compare as today. Remaining ties rank by rendezvous hash of
`(prefix, source relay)`, then of `(prefix, next hop)`. Broadcasts from one
source then spread across equal-cost paths, and a tie between sources resolves
identically everywhere. The chain hash and recency stay as the final
tie-breaks.

## Plan

`route_order` in `rs/moq-net/src/model/origin.rs` is anonymous, then `Cost`,
then chain length, then an FNV hash of prefix and whole chain, then newest
entry. The chain hash differs between relays for the same source, which is
what lets two relays pick different sources on a tie. Insert two keys after
chain length:

- the source: the first hop in the chain that has a beacon, or else the first
  hop;
- the next hop: the last hop in the chain.

Each is scored as a rendezvous hash with the prefix, highest first. The
beacon lookup arrives with
[beacons](/quest/m1/announce-tree/beacons.md); until then, use the first hop.
`best_route` on the data plane uses the same order, so subscriptions follow the
announced choice.

Hop count must stay additive per link and sit ahead of both hashes. Leave a
comment at `route_order` saying why, because the tree forwarding completeness
argument depends on it.

This changes tie-breaks for every moq-relay deployment, so call it out in the
PR. Tests:

- two sources at equal cost for one prefix: every relay in a three-relay line
  picks the same source;
- a prefix set over two equal-cost next hops splits between them;
- removing one next hop moves only the prefixes that ranked it first.

Extend `origin/announce_duplicate` with the new keys and compare against the
current numbers.

## Related

- [Tree forwarding](/quest/m1/announce-tree/forward.md) - relies on this
  ordering for completeness
