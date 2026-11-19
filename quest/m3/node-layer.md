# [L] Decide whether MoQ routes on a node layer

## Goal

A measured recommendation on whether moq-net keeps routing inside a trusted
domain, or layers on a node layer underneath that provides membership,
reachability, and per-node cost (from config, a control plane, or an overlay
such as Kixel's). MoQ would keep what only it can do: claims, warm routes,
and claims carrying a chain across trust boundaries, like BGP's AS path
between operators. The PR adds bake-off arms, a written design comparison,
and follow-up quests for the option the maintainer picks.

Why: the path vector took about thirty attempts since 2023, and much of its
hardest work serves forced mesh topologies, which belong to Kixel. Kixel
should run IP natively under MoQ rather than tunnel IP over MoQ, which is a
caching and fan-out layer and wrong for TCP/IP flows.

Non-goals: building the chosen design, and Kixel's overlay itself.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan on decoupling routing from
moq-net):

1. ✅ **Explore, don't build** (rejected: a pluggable router trait now, with no
   consumer named for a second implementation; shrinking moq's routing to
   operator-configured CDN topologies; extracting today's router as a crate
   for Kixel to reuse, since claims and Instances mean nothing to a TCP
   flow). The maintainer's framing: MoQ is the caching and fan-out layer,
   with a separate path or node layer below it. moq still needs multiple
   routes and to discover which CDN owns which broadcast, possibly BGP-like,
   with MoQ negotiating only between less trusted peers and anything routing
   inside a domain.
2. ✅ **Measured on the bake-off and written up** (rejected: a design doc with
   no measurements, since routing arguments here have passed review and
   then failed, as the four warm discounts did; answering in a session
   without a record).
3. ✅ **Arms** (`rs/moq-net/src/route/tests/bakeoff.rs`, the same meshes,
   failure schedules, and message, unfed, and loop metrics that
   [moq-dev/moq#8880](https://github.com/moq-dev/moq/pull/8880) judged the
   shipped rule by, which is the baseline):
   - Link-state inside a domain: relays flood link states and each computes
     shortest paths, which MoQ reads as next hop and cold cost, as Open/R
     does over a replicated store.
   - A flat dialable domain: any relay dials any other, membership and
     pairwise cost come from config, an API, or measurement, and each
     broadcast's tree comes from claims and warm routes alone, as LiveKit's
     mesh and Cloudflare's relays work.
   - A central controller written up but not simulated: B4 and Espresso
     style, routes from one place with fail-static over one of the arms
     above, so its behaviour is that arm's plus a stale window.
   (Rejected: a Babel underlay, close to today's path vector, and its
   feasibility condition cannot let a relay join a copy at a neighbour no
   closer to the publisher, as cache-aware routing found.)
4. ✅ **Requirements inside a domain**: candidates are judged against the 14
   requirements of [cache-aware multicast](/quest/m2/multicast/README.md),
   with requirement 6 (no single point of failure) kept and requirement 12
   narrowed inside one trusted domain to "nothing elects a leader, and
   competing claims resolve to the closest". A node may know its domain's
   whole topology, as OSPF, IS-IS, and Open/R do at this scale. Across trust
   boundaries all 14 stand (rejected: all 14 as written, which rules
   link-state out before it is measured, against requirement 14; relaxing 6
   too, which the controller write-up may argue for on evidence).
5. ✅ **The lite-07 cut does not wait**: the
   [route-free domain check](/quest/m2/lite07-routeless-domain.md) runs
   before it so the frozen wire commits to nothing, and this runs after.

Recommendations:

- The layering precedent is PIM-SM (RFC 7761): it builds trees from
  whatever unicast table sits below it, joins toward the next hop that
  table gives, and re-joins when the next hop changes, keeping no
  reachability of its own. A join here already stops at the first node
  carrying the Instance, so warm routes map onto that. The design should
  state the contract a node layer offers MoQ in those terms (membership,
  next hop and cold cost toward each relay, drain, and change events), so
  Kixel can build to it, and say what MoQ does when the two disagree.
- A node layer must expose per-node cost, not only flat reachability, or two
  readers behind one constrained link pull two copies. Of the overlays
  surveyed, only Babel exposes it; iroh, Yggdrasil, Tailscale, Nebula, and
  libp2p give flat reachability. moq owns its QUIC stack (the iroh backend
  left in [moq-dev/moq#7345](https://github.com/moq-dev/moq/pull/7345)), so
  an IP-level overlay under moq-quic fits better than one with its own
  transport.
- Measure on the drone and LAN cases too, with the node layer supplying
  their costs: if those cases move under a node layer, AGENTS.md's line
  that mesh networking is Kixel's and the multicast plan's "a mesh is the
  product this serves" stop conflicting. Propose which way to resolve that
  in the PR.
- Say what each option deletes from moq-net and the lite-07 draft (route
  state, floors, epochs, DEAD and DRAIN, their proofs) against what it adds.

## Related

- [The route-free domain check](/quest/m2/lite07-routeless-domain.md) - keeps lite-07 open to a node layer
- [Cache-aware multicast](/quest/m2/multicast/README.md) - the requirements and the shipped rule this measures against
- [Cluster routing](/quest/m3/cluster-routing/README.md) - the node routes warm routes rank against
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - warm routes, which stay MoQ's either way
