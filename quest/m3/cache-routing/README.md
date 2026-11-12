# Cache-aware routing

## Goal

A relay stays on a broadcast's path only while it has a local reader, feeds
two or more downstream sessions, or is its one consumer's cheapest cold
route. Otherwise its child bypasses it, so a broadcast with one viewer
crosses fewer relays. A relay joins a neighbor
that already carries a broadcast instead of pulling a second copy, so a LAN
of relays with their own uplinks pulls one copy over one uplink. Routing
stays loop-free under stale state, converges once demand and topology stop
changing, and sends no per-broadcast message for an idle broadcast.

Non-goals: steering clients to the relay that holds a broadcast; decisions
from measured load or RTT ([Link quality](/quest/m5/link-quality.md));
browsers serving copies they receive; a relay on neither consumer's cheapest
route becoming a shared stop; one copy across a multi-hop sparse overlay
rather than among direct neighbors; and creating links, since routing
chooses only among the sessions an operator configured.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan):

1. ✅ **Node routes are the cold cost, and optional per-broadcast warm routes
   are the warm cost.** The per-origin ROUTE metric of
   [Cluster routing](/quest/m2/cluster-routing/README.md) stays the cold
   cost, static and one per origin. A relay carrying a broadcast may
   advertise a warm route for it. This revises Cluster routing's non-goal of
   cache-dependent route prices. (rejected: per-broadcast forwarding with a
   cache-only subscribe and no advertisement; keeping routing cache-agnostic)
2. ✅ **A warm route is per neighbor and travels one hop.** Reopened by
   item 8. It goes only to
   direct neighbors and is never forwarded. B is warm toward N only while B
   has demand besides N's (a local reader or another downstream session),
   and withdraws once that demand has been gone for the copy's existing
   linger, with no new timer. B sends it only to neighbors whose cold metric
   for the origin is at least its own, since no other neighbor may use it
   (item 4). Without the per-neighbor rule, a relay whose reader left stays
   warm because it still feeds N, and N never skips it again. (rejected:
   broadcast-wide warmth, as the pre-#3225 design had; forwarding warmth k
   hops, which floods every warm transition)
3. ✅ **One cost per link, priced marginally.** Pulling through a warm
   neighbor costs the link alone, since that neighbor's cold metric is
   already paid; a cold neighbor costs the link plus its cold metric. A tie
   goes to the warm route, then to item 4's hash. A topology that wants both
   behaviors prices a link that skips one relay at more than one neighbor
   link and less than two: a cold broadcast then takes the skip, and once
   the skipped relay pulls for its own reader, its neighbor joins it. With
   uniform prices both cases tie and fall to the hash. (rejected: a warm and
   a cold price per link; a warm price the carrier seeds)
4. **Eligibility, which the proof must hold.** Reopened by item 8. A relay
   may pull broadcast P
   through neighbor N only when N's advertised route to the request's pinned
   origin has a newer seqno, or an equal seqno with
   `(cold metric, hash(P, N))` below `(feasibility distance, hash(P, self))`.
   A feasibility distance never exceeds what its node advertised at that
   seqno, so it strictly decreases along every forwarding edge even when an
   advertisement is stale: no loop, no rank, no hold timer. The pre-#3225
   rank was a cold cost each relay reported about itself, so a stale report
   could let a ring of relays all let go at once, which its 500 ms hold only
   narrowed. Route advertisements stay one selection per origin.
5. ✅ **Proof first:** model and sweeps before the implementation.
   (rejected: implementing with targeted fixtures only)
6. ✅ **The proof picks the wire placement.** The warm message is designed
   fresh rather than reviving ANNOUNCE_TAKEOVER, which
   [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) deleted in
   favor of instance continuity. The proof sizes it and
   picks lite-07 or an additive SETUP extension. (rejected:
   reusing ANNOUNCE_TAKEOVER's slot)
7. ✅ m3, after the m2 route layer it ranks on. (rejected: m2; m4) Amended
   2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet plan):
   ✅ the proof alone moves to m2, ahead of the lite-07 cut, and lands the
   message's draft section and codec there if it places it in lite-07
   (rejected: a design-only sizing quest in m2; leaving the proof in m3 and
   accepting a SETUP extension). The implementation stays in m3. This
   replaces item 6's rule that choosing lite-07 went back to the maintainer.

8. Reopened 2026-11-11 (maintainer, /quest-plan on the route redesign):
   items 2 and 4. In the maintainer's words: "I think we need warm routes
   propagating, that decision was made based on this flawed setup." ✅ **A
   warm route is per broadcast and propagates**, for as long as it is
   cheaper than the route its publisher's broadcasts share
   ([epic](/quest/m2/cluster-routing/routes/README.md) Plan item 26). A
   broadcast no relay carries rides the shared route and costs no message of
   its own. Item 4 rests safety on Babel's distance, which cannot let a relay
   join a copy at a neighbour that is no closer to the publisher: the join
   the Goal wants, and why item 2 stopped at one hop. What keeps a warm route
   free of loops and stale copies is
   [the prototype](/quest/m1/route-stale-copies.md)'s to
   measure, and the proof then holds that rule. Items 1 and 3 stand.
   [moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) proves items
   2 and 4 as first decided.

Prior art: warm/cold route prices
([moq-dev/moq#2925](https://github.com/moq-dev/moq/pull/2925)) were deleted
by prefix routes ([moq-dev/moq#3225](https://github.com/moq-dev/moq/pull/3225)),
and the pop-skipping questline (warm advertise, rank, peer reconfigure) was
pruned in [moq-dev/moq#4393](https://github.com/moq-dev/moq/pull/4393).
Related designs: PIM-SM's switch from a shared tree to a shortest-path tree
([RFC 7761](https://www.rfc-editor.org/rfc/rfc7761.html)), NHRP shortcuts
([RFC 2332](https://www.rfc-editor.org/rfc/rfc2332.html)), and Babel's
feasible successors ([RFC 8966](https://www.rfc-editor.org/rfc/rfc8966.html)).

This README's own work, once its children land:

- End-to-end tests over real sessions, time mocked. A chain of three relays
  plus a skip link, priced 3 and 5: a cold broadcast takes the skip, a reader
  on the skipped relay makes the far relay join it, and that reader leaving
  sends it back to the skip. Two LAN relays with their own upstream links and
  readers on both pull one copy over one uplink, with the owner spread across
  broadcasts by the hash.
- Docs: warm routes and how to price skip links in `doc/bin/relay/cluster.md`,
  and a section on the routing concept page once the
  [routing guide](/quest/m1/routing-guide.md) has added it.

## Required

- [Prove cache-aware routing](/quest/m2/cache-routing-proof.md) - warm routes and the eligibility rule hold in route-layer sweeps, and the wire placement is chosen

## Related

- [Cluster routing](/quest/m2/cluster-routing/README.md) - the node routes and feasibility distances this ranks on
- [Relays split horizon per session](/quest/m3/route-split-horizon.md) - per-session suppression of node routes, beside warm routes' per-neighbor rule
