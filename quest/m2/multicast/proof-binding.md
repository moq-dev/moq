# [L] Bind the route proof to the router

## Goal

The route proof's Rust binding runs `route::relay::Router`'s own functions,
so `just test route-proof` checks the router that ships.

## Plan

Found 2026-11-16 by [the proof](https://github.com/moq-dev/moq/pull/8904)
against the prototype; proposed by
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053).

- Order equal routes the same way in the proof and the router: the proof
  orders them by the neighbour, the prototype by its simulation's node index,
  and `route::relay::Router::reselect` by rank (drain, priority, cost), then
  an unmarked route first, then the next hop in use, then path length, then
  the sender's Node ID, then the neighbour key. Ordering by the neighbour's
  Node ID alone matches only if the router adopts it; otherwise give the
  binding the router's whole key.
- Record the chain as the claim instance's path: the announcing relay first,
  then each grant's exporter and adopter, so a node refuses a claim naming it
  in the role it would take. The prototype and the router record only the
  adopters, so they refuse fewer echoes than the proof assumes. This changes
  what a lite-07 claim's Path carries, so it lands before the lite-07 cut or
  is dropped.
- A claim the router adopts across a marked link, or one carrying the
  `Marked` flag, is announced under a second Node ID, the node's Relay with
  its Start plus 1 ([moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053)).
  The route to that identity is the node's own: marked, at cost 0, with the
  Path `[marked identity, node]`. The claim keeps its own cost, the giver's
  claim cost plus the adopting link's, and no Path of its own. Model both, or
  say why the proof's single identity per node covers them.
- Move the binding to the router: point `route/tests/algebra.rs` and
  `route/tests/sticky.rs` at the router's own functions for a node's own
  route, sending a route on, taking one in, ordering candidates, and listing
  them, regenerate `Vectors.agda`, and run `just test route-proof`. The claim
  instance first needs the router's precedence as its leading keys: a claim
  exactly at its path before a broader one shown there, then the newest
  epoch, decided over every claim of the Instance at that path whether or not
  a route reaches it, and only then the route joined with the claim's drain,
  priority, and cost. Neither the mechanized instance nor its Rust vectors
  cover precedence yet.

Public API: none. Wire: the chain item changes what a lite-07 claim's Path
carries.

## Related

- [Cache-aware multicast](/quest/m2/multicast/README.md) - the epic
- [What the route switch left](/quest/m2/multicast/switch.md) - where these came from
