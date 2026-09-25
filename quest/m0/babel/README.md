# Babel routing

## Goal

moq-lite-07 announcements drop the hop list. A route carries a source id, a
per-route sequence number, and its warm and cold cost, and relays stay
loop-free with Babel's feasibility condition
([RFC 8966](https://www.rfc-editor.org/rfc/rfc8966) section 3.5) instead of
path inspection. A reroute that leaves a relay's advertised cost unchanged no
longer propagates, and every announce stops paying for its path. lite-06 and
older, and the IETF cluster extension, keep today's hop lists.

Non-goals: fewer routes per relay (tree routing was dropped), and a
permanently mixed mesh. Mixing pre-07 and lite-07 relays is a rollout window.

## Plan

Today the hop list does three jobs: loop prevention (a relay drops a path
containing itself), request exclusion (never advertise or serve through the
requester, anywhere in the chain), and stitching identity (the first hop). Any
change to the chain emits an update, and a relay forwards that update to every
peer that isn't excluded. [Wildcard](/quest/m0/wildcard/README.md)'s Spread
quest moves identity onto the subscribe and fetch reply, so this line only
replaces the first two jobs.

### Wire (lite-07, still `moq-lite-07-wip`)

- ANNOUNCE_START carries `Source ID`, `Seqno`, and the warm and cold costs in
  place of `Hops`. The source is fixed for the life of an Announce ID: a new
  source is END and START.
- ANNOUNCE_UPDATE carries `Seqno` and costs.
- New ANNOUNCE_REFRESH (subscriber to publisher, on the announce stream):
  `Announce ID` and the wanted `Seqno`.
- An anonymous flag replaces the 0 Hop ID, so anonymous routes still rank last.
  The encoding is the implementer's call.
- The Cost Parameter's link cost is at least 1 and the wire cannot express 0
  (encode it minus one). Every hop then strictly raises both costs, which
  feasibility needs.
- [Announce compression](/quest/m1/announce-compression.md) lands first; this
  deletes its hop-tail half (`Hop Base`/`Hop Keep`) and keeps path compression.

### Routing

- A seqno covers one route: a source keeps one per announced prefix, so a
  refresh re-announces only the route that starved.
- A relay keeps a feasibility distance per (source, prefix): the best
  `(seqno, warm, cold)` it has advertised. It accepts a route only with a newer
  seqno, or the same seqno and a lexicographically lower cost. A saturated cost
  cannot drop, so it is infeasible, which is safe.
- A relay with no feasible route sends ANNOUNCE_REFRESH on the streams offering
  an infeasible one. A publisher that already holds a new enough seqno
  re-announces. Otherwise it forwards the request toward its own upstream, and
  the source bumps the route's seqno. At most one refresh per route per
  upstream is outstanding.
- Exclusion is adjacent-only: never advertise or serve a route back to the
  peer it arrived from. Deep exclusion goes with the chain.
- Selection keeps specificity, then anonymity, then warm and cold cost. The
  shortest-path tie-break goes away, since cost already grows every hop.
- An ingest relay mints a random source id for a publisher that declares none
  (lite-02/03, IETF without the cluster extension) and marks it anonymous.
- [Warm advertise](/quest/m1/pop-skipping/warm-advertise.md) re-originates the
  exact path as the relay's own source, so its warm-zero price is compatible.

### Bridge

Routes from a pre-07 or IETF peer take `hops[0]` (or a minted id) as source.
Routes sent to one carry a hop list the bridge synthesizes. The bridge may be
conservative, but it must never hold a persistent loop.

### Line work

This README owns the draft and the end-to-end proof:

- Lite draft Routing, ANNOUNCE_START/UPDATE/REFRESH, Cost Parameter, and the
  lite-07 changelog.
- `doc/concept/moq-lite.md`.
- A mixed ring of pre-07 and lite-07 relays that converges with no persistent
  loop, fails over when a mid-path relay dies, and recovers a starved route
  through REFRESH.

## Quests

- [Rust routing](/quest/m0/babel/rust.md) - moq-net's route model, the lite-07 codec, ANNOUNCE_REFRESH, and the pre-07/IETF bridge
- [JS codec](/quest/m0/babel/js.md) - `@moq/net` speaks the lite-07 route fields and answers ANNOUNCE_REFRESH as a source

## Required

- [Wildcard](/quest/m0/wildcard/README.md) - its Spread quest moves stitching identity onto the reply, which this line stops carrying in announcements

## Related

- [Local origin](/quest/m0/local-origin.md) - gives localhost workers locally ingested broadcasts without reading hop chains
- [Skip unchanged announce updates](/quest/m0/announce-update-dedupe.md) - the other half of cutting update churn
- [Announce counters](/quest/m0/announce-counters.md) - measures the saving
