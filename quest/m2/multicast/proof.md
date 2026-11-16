# [M] The route rule is proven to converge on any topology

## Goal

The route rule's algebra is instantiated against the published Agda library
for path-vector convergence, with every axiom discharged: how a node ranks
routes, how a route changes crossing a link, the mark's export filter, the
rule that a path naming its receiver is invalid, and the chain across a trust
boundary. The theorem then applies on any topology: from any starting state,
under loss, reordering, and duplication, the nodes reach one stable state. A
Rust property test asserts that the router's own rank and extend functions
meet the same axioms, a nightly lane checks the Agda, and
`rs/moq-net/verify/README.md` says what is proven and what is not.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): ✅ mechanized, checked nightly
and not in the merge gate, with TLA+ and sweeps beside it (rejected: a
written proof with property tests; TLA+ and sweeps alone). Today's checking
is exhaustive only at 2 to 3 relays with one failure, and the maintainer
ended the unbounded TLA+ attempt on 2026-11-13.

The theorem is Daggitt and Griffin's, "Formally Verified Convergence of
Policy-Rich DBF Routing Protocols" ([arXiv 2106.01184](https://arxiv.org/abs/2106.01184)),
Theorem 5: an increasing path algebra always converges. Its library is
[MatthewDaggitt/agda-routing](https://github.com/MatthewDaggitt/agda-routing);
pin it and Agda through Nix.

What to show, each a lemma about one message handler:

- Selection is a total order with a fixed tie-break.
- No change across a session makes a route preferred to what it was: adding
  the link's cost, taking the lower priority, keeping a drain, appending the
  hop. A filter maps a route to invalid.
- A route whose path names the receiver is invalid, and a path grows by
  exactly the hop it crossed.
- What a node advertises is a function of the latest it heard from each
  neighbour. The dead-hop withdrawal is extra: show it removes only routes
  the iteration would remove, so it only arrives sooner.
- The chain is the same algebra at the grain of trust boundaries.

Measured 2026-11-15 by the [bake-off](https://github.com/moq-dev/moq/pull/8880),
pending the maintainer's pick there. The rank is the epic's: not draining,
the lowest priority on the path, the summed cost, each link adding at least
1, then the next hop in use, the shorter path, and the neighbour. Three more
lemmas if its recommendations stand:

- The session floor is a filter: a path naming a pair of nodes at or below the
  highest session known closed between them maps to invalid, and the set only
  grows, so it removes only routes the iteration would remove.
- A named drain sets a route's drain from its path and the nodes known
  draining, and the drain still travels in the offer, so the order stays one
  order.
- A claim ranks by the route to its announcer joined with its own drain,
  priority, and cost; show an echo across two grants never beats the claim it
  echoes.

A node advertising once per burst changes no fixed point.

State plainly what the theorem does not give, and where each is covered:

- No loop at every instant: [requests](/quest/m2/multicast/requests.md) fail
  closed instead.
- A cost that depends on who subscribed:
  [warmth](/quest/m2/multicast/warmth.md) argues it separately.
- Liveness under churn that never stops.
- That the tree is loop-free once routes settle: one paragraph, since next
  hops toward one origin node at a fixed point cannot cycle and a request
  names its origin node.

TLA+ stays for the session, subscription, and failover state machines, where
orderings matter. `RouteHandoff.tla` describes the rule being replaced and
goes with it in the [switch](/quest/m2/multicast/switch.md). Report how long
the Agda check takes.

Public API: none. Wire: none.
