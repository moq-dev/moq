# Babel routing

## Goal

A route change stops flooding the cluster with announce updates, and the
routing algorithm that achieves it is chosen by simulation before any wire
format is committed. Babel is the leading candidate: moq-lite-07 announcements
drop the hop list, and a route carries a source id, a per-route sequence
number, and its warm and cold cost. Relays stay loop-free with Babel's
feasibility condition ([RFC 8966](https://www.rfc-editor.org/rfc/rfc8966)
section 3.5) instead of path inspection. lite-06 and older, and the IETF
cluster extension, keep today's hop lists.

Non-goals: fewer routes per relay (tree routing was dropped), and a
permanently mixed mesh.

## Plan

Today the hop list does three jobs: loop prevention (a relay drops a path
containing itself), request exclusion (never advertise or serve through the
requester, anywhere in the chain), and stitching identity (the first hop). Any
change to the chain emits an update, and a relay forwards that update to every
peer that isn't excluded. [Wildcard](/quest/m0/wildcard/README.md)'s Spread
quest moves identity onto the subscribe and fetch reply, so this line only
replaces the first two jobs.

### Gate

The [Routing simulator](/quest/m0/babel/simulator.md) decides whether the wire
quests below go ahead as written. Babel proceeds if, on the same workloads, it
sends clearly fewer announce messages than today's path vector, never holds a
persistent forwarding loop, and bounds transient loops and unavailability. If
the simulator shows fan-out driven by topology dominates instead (every relay
re-advertising every route to every peer), propose a line that separates relay
topology from broadcast reachability, and replace the wire quests with it.

Babel's weak spot is the MoQ common case. RFC 8966 section 2.7 does not
guarantee loop freedom when several routers originate the same prefix, and
MoQ has overlapping prefixes, several publishers, warm relays that
re-originate, and per-request pool selection. Lite SUBSCRIBE carries no path,
so a forwarding loop is not caught at subscribe time; it stalls the
subscription until the route changes. The proof obligation is on the actual
subscribe and fetch forwarding decisions, not on converged announce tables.

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
- Feasibility state outlives the route (RFC 8966 section 3.7.3): an entry is
  dropped on a timer long enough for any delayed advertisement of that seqno
  to have expired, never because the last route went away.
- A withdrawn prefix is held unreachable (RFC 8966 section 3.5.4) until a
  feasible route returns or every neighbour has stopped routing through this
  relay, so a covering prefix cannot take over and loop back.
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

### Rollout

The cluster switches to lite-07 routing as a whole; pre-07 and IETF sessions
stay at its edges, where a route from one takes `hops[0]` (or a minted id) as
its source and a route to one carries a single-hop list. Mixed pre-07 and
lite-07 transit inside the cluster is out of scope. If a fleet cannot switch
at once, the bridge is designed and proven in the simulator before either
wire quest starts.

### Line work

This README owns the draft and the end-to-end proof:

- Lite draft Routing, ANNOUNCE_START/UPDATE/REFRESH, Cost Parameter, and the
  lite-07 changelog.
- `doc/concept/moq-lite.md`.
- A lite-07 ring that converges with no persistent loop, fails over when a
  mid-path relay dies, and recovers a starved route through REFRESH.

## Quests

- [Routing simulator](/quest/m0/babel/simulator.md) - a deterministic simulator runs today's path vector, Babel, and a topology split under the same workloads, and decides whether the wire quests go ahead
- [Rust routing](/quest/m0/babel/rust.md) - moq-net's route model, the lite-07 codec, ANNOUNCE_REFRESH, and the cluster edge
- [JS codec](/quest/m0/babel/js.md) - `@moq/net` speaks the lite-07 route fields and answers ANNOUNCE_REFRESH as a source

## Related

- [Local origin](/quest/m0/local-origin.md) - gives localhost workers locally ingested broadcasts without reading hop chains
- [Skip unchanged announce updates](/quest/m0/announce-update-dedupe.md) - the other half of cutting update churn
- [Announce counters](/quest/m0/announce-counters.md) - measures the saving
