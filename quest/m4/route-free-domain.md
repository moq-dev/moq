# [M] Relays run route-free over a node layer

## Goal

An operator or application whose relays already reach each other through a
node layer underneath (an overlay such as Kixel's, or a provider network)
can run them as a route-free domain: every in-domain session exchanges claims
with no ROUTE, DEAD, or DRAIN, under split horizon, and the docs say what
that mode gives up. Non-goals: a router trait or a second routing rule
inside moq-net, and the node layer itself.

## Plan

From the node-layer bake-off
([moq-dev/moq#9149](https://github.com/moq-dev/moq/pull/9149), 2026-11-18,
decided under the maintainer's afk rule and open to review there): the
shipped path vector stays moq-net's only routing rule. Link state cost 6 to
60 times its messages with no fewer unfed readers. A flat domain sent no
route message for link events its network absorbed, but noticed a lost path
only when keep-alives ran out (27,000 unfed relay-rounds for a partition at
32 relays against the path vector's 293), held a session per pair of relays
(992 at 32), and without warm routes carried up to 30 copies of a broadcast
over one link where the path vector carries 1.

What exists: the frozen lite-07 wire already carries this mode
(`rs/moq-net/tests/integration/routeless_domain.rs`). A session wired one way
declares no Node ID, and marking every in-domain link `upstream` gives split
horizon, so a claim ends everywhere at once.

What this adds, as recommendations:

- A per-link option, not a session pair per direction, that keeps a two-way
  session from declaring a Node ID, with the link marked. Name it under the
  link's existing settings beside `upstream` and `cost`, in Rust, `js/net`
  (which only answers, so likely nothing), and `moq-relay`'s cluster config.
- Docs in `doc/bin/relay/cluster.md`: when to use it, and what it gives up.
  No detour inside the domain, since the network routes; failure noticed by
  keep-alives unless the node layer reports it; and fan-out only through warm
  routes ([cache-aware routing](/quest/m3/cache-routing/README.md)) choosing
  among every relay by configured cost.
- A real-session test through `moq-relay` with the option set.

## Waiting

- Maintainer: keep this quest only if Kixel will run MoQ this way. No
  consumer in this repository needs it, and an agent cannot commit Kixel's
  plans. Check: Kixel's quest tree plans MoQ over its overlay. Recommendation:
  keep if Kixel's tunnels move to an IP overlay with media on top, as planned
  in this session; otherwise delete, since one-way sessions already serve a
  library user.
- Maintainer: settle who owns mesh routing. AGENTS.md says mesh networking is
  Kixel's, while [cache-aware routing](/quest/m3/cache-routing/README.md) and
  [cache-aware multicast](/quest/m2/multicast/README.md) target drones and
  self-forming meshes. Options: meshes run route-free over Kixel's overlay and
  moq's path vector serves operator-configured relay graphs (recommended: the
  bake-off found a node layer only pays where something underneath already
  routes, which is Kixel's case); or moq's path vector keeps serving meshes,
  and AGENTS.md changes. An agent changes no agent instruction without the
  maintainer's agreement.

## Related

- [Cache-aware routing](/quest/m3/cache-routing/README.md) - warm routes, which a flat domain needs to fan out
