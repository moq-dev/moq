# [S] Cost across scopes

## Goal

A written rule, in `doc/concept/transport.md` and the drafts, for how a route
through a P2P peer compares with the relay's own route, such that a peer
already carrying a broadcast wins and an idle peer never has traffic pulled
through it. The rule is implementable by the Rust and JS origins with the
existing `Cost { warm, cold }` and the per-link price a publisher declares in
SETUP.

## Plan

This is a planning quest; it produces the rule and rewrites the
implementation quests that depend on it.

What is settled today: a publisher declares an egress price in SETUP on
lite-06 and the receiver charges it per link, with local policy able to
override the peer's declaration; `warm` and `cold` accumulate per link and
compare in that order; a chain containing an anonymous hop loses to any
identified chain; Rust migrates a live subscription only to a route with the
same first hop.

What is not: costs live in one scope, a mesh pricing its own links. A P2P
link is neither free (peer uplink, reliability) nor the CDN's egress, and
nothing flattens `warm` when an origin already carries the content, so a tab
forwarding the relay's route at the relay's price plus its own ties the relay
and loses on chain length. Candidate rules to weigh, with the numbers from
the [harness](/quest/next/p2p/harness.md) where they exist:

- a warm discount: an origin actively receiving a broadcast re-announces it
  at `warm` 0 and `cold` unchanged, over every session, in both languages,
  which is what the two magnitudes were designed for;
- explicit prices per scope: the relay's client-facing link priced as egress,
  the P2P link priced by the application, and a documented comparison between
  them;
- a scope tag on the route, so a P2P route is compared with a relay route by
  a rule rather than by subtraction.

Decide who sets the P2P link price (the application, per `Peers` knob), what
the relay's client-facing default is, whether the first-hop migration gate
holds when the first hop is a publisher tab, and whether the warm discount
belongs in the mesh too. Write the rule beside route selection in
`doc/concept/transport.md`, mirror it in `draft-lcurley-moq-lite.md` and
`draft-lcurley-moq-cluster.md` where the wire carries it, and pass
`just drafts check`. Then update [watch](/quest/next/p2p/watch.md) and
[transit](/quest/next/p2p/transit.md) with the chosen rule and open the
implementation quest it needs.

## Related

- [Route cost in the JS origin](/quest/next/route-cost.md) - the ranking that consumes the rule
- [PoP skipping](/quest/next/pop-skipping/README.md) - the mesh-side use of warm versus cold
