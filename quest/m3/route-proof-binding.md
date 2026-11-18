# [L] Bind the route proof to the router

## Goal

The route proof's Rust binding runs `route::relay::Router`'s own functions,
so `just test route-proof` checks the router that ships.

## Plan

Found 2026-11-16 by [the proof](https://github.com/moq-dev/moq/pull/8904)
against the prototype; proposed by
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053).

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053#issuecomment-6975349236)):
✅ kept, and it follows the lite-07 cut, since none of it is wire; the
chain change, which is, lands before the cut as
[its own quest](/quest/m2/multicast/claim-chain.md) (rejected: the whole
binding before the cut; dropping the chain change).

- Order equal routes the same way in the proof and the router: the proof
  orders them by the neighbour, the prototype by its simulation's node index,
  and `route::relay::Router::reselect` by rank (drain, priority, cost), then
  an unmarked route first, then the next hop in use, then path length, then
  the sender's Node ID, then the neighbour key. Ordering by the neighbour's
  Node ID alone matches only if the router adopts it; otherwise give the
  binding the router's whole key.
- A claim the router adopts across a marked link, or one carrying the
  `Marked` flag, is announced under the node's marked Node ID: its own Relay
  and Start with the typed Marked field set, the same process holding no
  session ([moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053)).
  The route to that identity is the node's own: marked, at cost 0, with the
  Path `[marked identity, node]`, whose marked hop counts toward the 32. The
  claim keeps its own cost, the giver's claim cost plus the adopting link's,
  and its own chain, but no route Path of its own. Model both, or say why the
  proof's single identity per node covers them.
- Move the binding to the router: point `route/tests/algebra.rs` and
  `route/tests/sticky.rs` at the router's own functions for a node's own
  route, sending a route on, taking one in, ordering candidates, and listing
  them, regenerate `Vectors.agda`, and run `just test route-proof`. The claim
  instance first needs the router's precedence as its leading keys: a claim
  exactly at its path before a broader one shown there, then the newest
  epoch, decided over every claim of the Instance at that path whether or not
  a route reaches it, and only then the route joined with the claim's drain,
  priority, and cost. Neither the mechanized instance nor its Rust vectors
  cover precedence yet. Bind the chain as
  [the claim chain](/quest/m2/multicast/claim-chain.md) leaves it.

Public API: none. Wire: none.

## Related

- [Cache-aware multicast](/quest/m2/multicast/README.md) - the route layer it proves
- [Claim chain](/quest/m2/multicast/claim-chain.md) - the wire half, before the cut
