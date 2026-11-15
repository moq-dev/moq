# Cache-aware multicast routing

## Goal

moq-net gets one routing design, proven and measured before it replaces
lite-07's route layer. A subscription joins the nearest node already carrying
the same Instance, so no link carries a group twice, and moves between routes
of equal Instance without the reader noticing. State and messages grow with
forwarding nodes and demanded broadcasts, not publishers. There is no leader,
global view, or single point of failure, on CDN tiers and on a self-forming
mesh alike.

Non-goals: choosing, dialing, and pricing links from measured quality, which
is Kixel's; browsers as transit; stitching different Instances together; and
moq-transport routing parity beyond each node using its nearest publisher.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan), after an audit of the lite-07
route layer as built, the history of about thirty routing attempts since
2023, and a survey of the literature.
[Routes and announces](/quest/m2/cluster-routes/README.md) holds the rule
this replaces and its measurements.

The maintainer's requirements, which every child is judged against:

1. A subscription resumes only onto equal identity.
2. A publisher may hold two connections (a GOAWAY handover, redundant
   ingest), and routes switch between them seamlessly.
3. Some connections do not proxy: an edge passes its clients to its cores
   and its cores' routes to its clients, never one core's routes to another,
   and a leaf, such as a relay on a camera, advertises only what its host
   serves.
4. The specification stays simple, reusing messages and concepts.
5. Traffic grows as little as possible with the cluster.
6. No single point of failure.
7. It is proven to work.
8. Routing prefers warm caches: a route has a cold cost and a warm cost, and
   routes coalesce onto a node that says it carries a broadcast, so no link
   carries the same data twice.
9. Messages scale with the cluster: streams of messages, never one blob.
10. One stream carries what must stay ordered.
11. moq-transport is supported to a degree: identity does not carry, but two
    publishers announcing one name on opposite sides of the network work.
12. No global view. With competing claims each node uses its closest, and
    nothing elects a leader.
13. A secondary route may carry metadata only, so a stalled primary is found
    without waiting on QUIC keep-alives.
14. No approach is assumed: earlier attempts are evidence, not constraints.

What the audit found of today's layer: 6 and 12 hold, and 1 holds with no
model covering it; 4, 8, and 13 do not hold (16 message types on the announce
stream and about 57 concepts against lite-06's 5 messages, no cache term in
any ranking, and a payload stall on live sessions that is never recovered);
the rest hold in part. Exhaustive checking stops at 3 relays and one session
close, safety only, and rests on a timer that outlasts every stale offer,
which a 1 s doubt does not when a dead relay's neighbours notice up to 3 s
apart. Route state and churn follow publishers.

Settled:

1. ✅ **Replace, not repair.** Today's ROUTE, DEAD, hold, and doubt wire is a
   candidate like any other (rejected: keeping the path router as the base
   and adding cache awareness and the metadata secondary on top, which leaves
   requirement 4 unmet and state following publishers; moq also owning link
   formation and pricing for the mesh).
2. ✅ **lite-07 carries it, and the cut waits** (rejected: lite-08, which
   publishes a wire we expect to replace and makes two route layers
   interoperate for a release; lite-07 without a route layer, a large strip
   that leaves lite-07 clusters with lite-06's hunting). The route stream and
   the acceptance fixtures still land: they are the baseline the
   [bake-off](/quest/m2/multicast/bakeoff.md) measures against, and one
   announce stream per side is wanted by any design. The quests that only
   polish holds, doubt, or the path router's indexes wait on the bake-off.
3. ✅ **A routing node is a node that forwards and is trusted to; claims,
   not routes, cross a trust boundary.** Every CDN relay and every mesh
   device routes under its own Node ID. An endpoint that never forwards (an
   app, an encoder, a browser) is local to the relay it connects to, which
   announces its broadcasts under the relay's Node ID. Meshes sharing a fleet
   credential are one routing domain. Across a scoped grant the receiving
   node announces the claim again under its own Node ID with a chain of the
   nodes that did so, and refuses a claim whose chain names it, which is the
   Lite-06 Chain made general. The maintainer first answered "we need every
   publisher so meshes can communicate with other meshes"; the chain covers
   that across any number of scoped hops, where a qualified identity crosses
   one. Redundant ingest is two relays claiming one Instance. This removes
   the Route Key Parameter with its proof, qualified Node IDs, and routes per
   prefix, and reverses Plan items 18 and 26 of
   [Routes and announces](/quest/m2/cluster-routes/README.md) (rejected:
   every publisher an origin node, as today; measuring both, which doubles
   the prototype).
4. ✅ **Evidence, not timers.** The route layer is a plain path vector with
   the dead-hop withdrawal as an accelerator: no ROUTE_HOLD, no doubt, and no
   dead hop remembered past the routes that name it. Routing state is a
   hint. A request is confirmed only once its upstream is, data is identified
   by Instance, track, group, and frame, and a stale or stalled route is left
   on evidence ([requests](/quest/m2/multicast/requests.md),
   [route health](/quest/m3/route-health/README.md)). What a hold protected,
   a claim flickering while routes settle, becomes a reader's local debounce
   from unavailable to offline, never on the wire. This reverses items 19 and
   27's doubt (rejected: keeping holds and doubt with the doubt tied to the
   idle timeout; a doubt lifted by an epoch the surviving node floods).
5. ✅ **Three ways to find a warm copy are measured before one is picked**
   ([warmth](/quest/m2/multicast/warmth.md)): joins that stop at the first
   node carrying the Instance and nothing more; that plus a neighbour saying
   on each SUBSCRIBE answer whether it carries the Instance and at what cost;
   and carrying nodes advertising it. One constraint binds all three: a warm
   cost is strictly positive and summed from the source along the path the
   data takes. A warm discount lets two warm relays prefer each other, which
   happened four times here.
6. ✅ **One mark per link.** A route learned over a marked link is never sent
   over another marked link, as `upstream` works today. An edge marks its
   core links and a leaf marks every relay link (rejected: a strict mark that
   sends only what the node originates, which ends a LAN peer publishing
   through a neighbour whose uplink survived; both switches).
7. ✅ **The proof is mechanized, with TLA+ and sweeps beside it**
   ([proof](/quest/m2/multicast/proof.md); rejected: a written proof with
   property tests, the kind of argument that passed review here and failed
   for the echo, the deferred echo, and belief by distance; TLA+ and sweeps
   alone, which say nothing about an arbitrary topology).
8. Carried from the route-health epic deleted 2026-11-09
   ([moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423)), whose
   decisions the maintainer made 2026-10-13 and 2026-11-02 and nothing
   reversed: metadata-only delivery is a mode of SUBSCRIBE, never its own
   message; a stall is found by comparing the two routes, never by silence;
   and a demoted route never takes a track back while the working one
   delivers.

The shape those decisions give, for the children to confirm or improve; each
departure is a decision to list in its PR:

- **Identity.** A path and an Instance name content, and a request names the
  Instance. A relay holds one upstream subscription per Instance and track,
  whichever origin node its downstream requests named, so a join stops at the
  first node already carrying it.
- **Reachability.** One route per forwarding node: its path, drain, priority,
  and cost. A node never uses a route whose path names it. Rank is not
  draining, then the lowest priority on the path, then the summed cost, each
  link adding at least 1, so extending a route never improves it.
- **Claims.** A prefix, Instance, Epoch, and cost, announced by a forwarding
  node for itself and its local endpoints, ordered by the node's revision and
  certified by floors as today, which measured no revival and no false END.
- **Boundaries.** The chain of item 3.
- **Requests.** A request names the origin node its first hop chose, so
  relays that rank differently never loop it, and fails closed.

Lessons from the history that bind every child:

- A fact carries its own order: a revision, a path, a withdrawal that names
  its cause. Holds, quarantines, hold-downs, and split horizon never stopped
  a stale copy reviving a withdrawn fact.
- Every input to a rank travels in the offer and never improves when a route
  is extended. An input that depends on demand needs its own argument.
- Each side says everything that must stay ordered on one stream.
- A failure costs messages per session or per forwarding node, never per
  broadcast or per publisher behind it.
- Nothing ships unexercised: a loop check that never fired cost 32 bounces
  per announce in production.

## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the rule this replaces, its contract, and its measurements
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - implements the warmth pick
- [Route health](/quest/m3/route-health/README.md) - the metadata-only secondary
