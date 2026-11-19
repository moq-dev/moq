# Cache-aware multicast routing

## Goal

moq-net has one routing design, proven and measured, as lite-07's route
layer. A subscription joins the nearest node already carrying
the same Instance, so no link carries a group twice, and moves between routes
of equal Instance without the reader noticing. State and messages grow with
forwarding nodes and demanded broadcasts, not publishers. There is no leader,
global view, or single point of failure, on CDN tiers and on a self-forming
mesh alike.

Non-goals: choosing, dialing, and pricing links from measured quality, which
is Kixel's; browsers as transit; stitching different Instances together;
moq-transport routing parity beyond each node using its nearest publisher;
and a moq-transport node between two of our relays.

## Plan

Where it stands (2026-11-18). The route layer shipped:
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) put every
lite-07 session in Rust and `js/net` on `route::relay::Router`, rewrote the
draft's Routing, made `js/net` an endpoint, and deleted the path rule with
its holds, doubt, route key, qualified Node IDs, and routes per prefix;
[moq-dev/moq#9052](https://github.com/moq-dev/moq/pull/9052) made a relay
wait for a claim's route before answering a request for it; and
[moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053) made the
test-only route API crate-private, kept adopted claims off upstreams under
a marked Node ID, and marked the lite-06 churn mitigations; and
[moq-dev/moq#9082](https://github.com/moq-dev/moq/pull/9082) made a claim's
chain name its announcing relay, then each grant's exporter and adopter.
Its proof is
`rs/moq-net/verify/route-algebra/`, and the quests written against the old
wire were retargeted or deleted.
[moq-dev/moq#9111](https://github.com/moq-dev/moq/pull/9111) ran the cases no
test covered on real sessions, and
[moq-dev/moq#9154](https://github.com/moq-dev/moq/pull/9154) fixed the first
flaw it found: a front now rides through a lost claim's debounce, so a reader
resumes on a detour learned after its session closed. The lite-07 cut still
waits on this epic's children, the other flaw,
[a route moving behind one next hop resending a group](/quest/m2/multicast/resend.md),
whose fix changes the wire,
and two flaws the mesh replay found,
[feeding an old relay's subscription mid-roll](/quest/m2/multicast/roll-echo-stall.md)
and [a lite-07 front that stops delivering after it switches copies](/quest/m2/multicast/splice-stall.md);
and on
[route layer acceptance](/quest/m2/cluster-routes/acceptance.md)'s
scoped subscriber whose grant is too wide to ask for. It does not wait on
[warmth's port](/quest/m3/cache-routing/select.md): its wire joins lite-07
only if the port has landed and passed the mesh replay when the cut is made,
and otherwise moves to the first wip version after it (maintainer,
2026-11-19). The port is measured and not yet built, pending the maintainer
(the coordinating session, under the maintainer's delegation, 2026-11-19).
After the cut come
[route health](/quest/m3/route-health/README.md) and
[outbound federation](/quest/m3/cluster-routing/federation.md).

Decided 2026-11-15 (maintainer, /quest-plan), after an audit of the lite-07
route layer as built, the history of about thirty routing attempts since
2023, and a survey of the literature.
The rule this replaced, its contract, and its measurements are in
`git show 4e73451999:quest/m2/cluster-routes/README.md`.

The maintainer's requirements, which every child is judged against:

1. A subscription resumes only onto equal identity.
2. A publisher may hold two connections (a GOAWAY handover, redundant
   ingest), and routes switch between them seamlessly. Both serve one
   Instance, so redundant encoders are synchronized.
3. Some connections do not proxy: an edge passes its clients to its cores
   and its cores' routes to its clients, never one core's routes to another,
   and a leaf, such as a relay on a camera, advertises only what its host
   serves.
4. The specification stays simple, reusing messages and concepts.
5. Traffic grows as little as possible with the cluster.
6. No single point of failure.
7. It is proven to work.
8. Routing prefers warm nodes: a route has a cold cost and a warm cost, and
   routes coalesce onto a node that already has demand for a broadcast, so
   no link carries the same data twice. A CDN join stops at no pop without
   demand, and drones with an uplink each settle on one that reaches the CDN.
9. Messages scale with the cluster: streams of messages, never one blob.
10. One stream carries what must stay ordered.
11. moq-transport is supported to a degree: identity does not carry, but two
    publishers announcing one name on opposite sides of the network work. A
    moq-transport node is always an endpoint.
12. No global view. With competing claims each node uses its closest, and
    nothing elects a leader.
    Narrowed 2026-11-18 inside one trusted domain, for judging node layers
    under MoQ: a node may know its domain's topology, but nothing elects a
    leader and competing claims resolve to the closest. The node-layer
    bake-off ([moq-dev/moq#9149](https://github.com/moq-dev/moq/pull/9149))
    kept the path vector: link state sent more route messages on every link
    or relay failure but a 12-relay partition (up to 60 times at 32 relays;
    a publisher moving costs it claim floods, not route messages), with fewer readers unfed after a partition or a hub dying
    slowly but more after a transit dies, and held fewer copies of each
    claim. A flat domain over a network that routes underneath sends nothing
    for link events that network absorbs, but notices a partition only by
    keep-alives and fans out only with warm routes across every pair of
    relays.
13. A secondary route may carry metadata only, so a stalled primary is found
    without waiting on QUIC keep-alives, where a node opts in.
14. No approach is assumed: earlier attempts are evidence, not constraints.

What the audit found of the path rule lite-07 then ran: 6 and 12 hold, and 1 holds with no
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
   [bake-off](https://github.com/moq-dev/moq/pull/8880) measures against, and one
   announce stream per side is wanted by any design. The quests that only
   polished holds, doubt, or the path router's indexes went with it.
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
   dead hop remembered past the routes that name it (that last clause
   reversed 2026-11-16 by item 13: a session floor is kept for good).
   Routing state is a hint. A request is confirmed only once its upstream is, data is identified
   by Instance, track, group, and frame, and a stale or stalled route is left
   on evidence ([requests](https://github.com/moq-dev/moq/pull/8881),
   [route health](/quest/m3/route-health/README.md)). What a hold protected,
   a claim flickering while routes settle, becomes a reader's local debounce
   from unavailable to offline, never on the wire (narrowed 2026-11-16 by the
   coordinator and confirmed by the maintainer's merge pick of
   [moq-dev/moq#8901](https://github.com/moq-dev/moq/pull/8901): the route
   exchange between routing nodes keeps no timer, and an endpoint or scoped
   neighbour is told what the reader shows, so it hears a lost claim
   unavailable until the debounce runs out). This reverses items 19 and
   27's doubt (rejected: keeping holds and doubt with the doubt tied to the
   idle timeout; a doubt lifted by an epoch the surviving node floods).
5. ✅ **Three ways to find a warm copy are measured before one is picked**
   ([warmth](https://github.com/moq-dev/moq/pull/8903)): joins that stop at the first
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
   ([proof](/rs/moq-net/verify/route-algebra/README.md); rejected: a written proof with
   property tests, the kind of argument that passed review here and failed
   for the echo, the deferred echo, and belief by distance; TLA+ and sweeps
   alone, which say nothing about an arbitrary topology).
8. Carried from the route-health epic deleted 2026-11-09
   ([moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423)), whose
   decisions the maintainer made 2026-10-13 and 2026-11-02 and nothing
   reversed: a stall is found by comparing the two routes, never by
   silence; and a demoted route never takes a track back while the working
   one delivers. Reversed 2026-11-16 (maintainer, on
   [moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882)): a metadata
   subscription was a mode of SUBSCRIBE switched in place, and is now a
   subscription of its own with a fixed mode
   ([metadata-only subscriptions](/quest/m3/route-health/meta.md)).

Decided 2026-11-15 (maintainer, answering the requirements the plan's review
flagged as costly, on
[moq-dev/moq#8873](https://github.com/moq-dev/moq/pull/8873)):

9. ✅ **Warm means demand, and two cases define requirement 8.** A node is
   warm for an Instance while something downstream of it asks for it, not
   because groups sit in its cache: "downstream demand instead of a yes/no
   cache state". Pop skipping: a join stops at no pop without demand. Drones
   that "self-elect who is the one who reaches out to the CDN": with an
   uplink each, "the warm cost to a drone beats the cold cost to the CDN"
   and one uplink carries the broadcast (rejected: narrowing 8 to joins that
   stop at a carrier already on the path, which delivers neither case).
   [Warmth](https://github.com/moq-dev/moq/pull/8903) measured both.
10. ✅ **A moq-transport session is an endpoint, never transit.** Its wire
    has no path, chain, or Instance, so a moq-transport relay announcing
    back a name it learned from us cannot be told from a nearer publisher.
    That is unsupported: it must cost bounded work and touch no other
    broadcast ([requests](https://github.com/moq-dev/moq/pull/8881)), and nothing
    detects it (rejected: ignoring a claim for a name already announced to
    that session, which also hides a real second publisher behind it).
11. ✅ **Redundant ingest is one Instance from synchronized encoders.** Two
    connections claiming one Instance serve the same frames under the same
    group and frame numbers, and a publisher that restarts hard mints a new
    Instance, as the draft already requires. Nothing checks it: a pair that
    breaks it hurts its own broadcast. Encoders that are not synchronized
    use two Instances, and a move between them is a restart the reader sees;
    our importers are that case, since same-epoch importers were dropped
    2026-10-19. moq-transport never gets redundant ingest: it has no way to
    flag an encoder restart, so every claim over it is a minted Instance.
12. ✅ **The metadata secondary is opt in**
    ([route health](/quest/m3/route-health/README.md)): a mesh on links that
    stall wants it, and the CDN's own tiers run without one (rejected: a
    standby wherever a second route exists, which about doubles a track's
    subscription state across the cluster).

Decided 2026-11-16 (maintainer, "go for it" on all three, on
[the bake-off](https://github.com/moq-dev/moq/pull/8880#issuecomment-6911055514),
whose tables hold every number here; 32 relays on CDN tiers with 2
publishers, in route messages, unless noted):

13. ✅ **The new rule replaces today's, with five additions, none a timer**
    (rejected: the rule as written, which hunts through 6,625,456 for a
    partition against today's 579 and 1,368,094 for a relay that drains and
    closes; keeping today's rule, which leaves readers unfed about 1 s when
    a link closes beside a detour, grows with publishers to 73,472 selected
    routes at 256, and cannot cross two scoped grants). With all five a
    partition costs 690, a link closing feeds every reader at once, and a
    publisher leaving changes no route. No arm showed a false END or a
    revival.
    - `floor`: per pair of relays, a node keeps the highest session it knows
      closed and refuses any path naming that pair at or below it, for good.
      This reverses item 4's "no dead hop remembered past the routes that
      name it": that clause is what the rule hunted through, a stale offer
      arriving after the hop was forgotten (rejected: `quiet`, holding
      adverts until neighbours agree, which can wait for good and cost 4,641
      against 2,037 as written for a relay dying on a 12-relay chorded ring;
      `agree`, refusing a route a neighbour's own offer contradicts, a small
      gain `floor` covers).
    - `epoch`: a relay's identity carries a start that only grows, and a
      session orders by its relay's start and then its number, so a restart
      replaces a pair's floor where it added one: 4 pairs after each of 8
      restarts against 4 more per restart, with no row of any table changed.
    - `batch`: a node advertises once it has read what was delivered, not
      after every message. `floor` alone still spent 28.8 million on one
      GOAWAY of a 32-relay chorded ring; with `batch`, 5,065.
    - `drain`: a node that sent GOAWAY is named once per neighbour in a
      message of its own, as a closed session is: 5,065 to 723.
    - `claimed`: a node offers a route to itself only once it announces a
      claim, and from then on until it restarts, so a route is per announcing
      relay. Building the mesh for one publisher fell from 6,576 to 207,
      under today's 221, and the partition from 7,549 to 690. A publisher's
      first claim on a relay then costs a route at every node (201 for a
      move). The route stands after the last claim leaves (rejected:
      withdrawing it, which races the claim's own end and would show a
      reader offline where the publisher ended cleanly).
    - Order at equal rank, decided by the coordinator on 2026-11-16 while
      the maintainer was away and confirmed that day by their merge pick of
      [moq-dev/moq#8904](https://github.com/moq-dev/moq/pull/8904): a route with no
      marked link on its path beats a marked one, then the next hop in use,
      the shorter path, and the neighbour. A relay holding two routes of
      equal rank, one learned over a marked link, had kept whichever came
      first, so a neighbour behind its own marked link was reachable or not
      by arrival order, and under a fixed order by the neighbours' keys
      ([proof](/rs/moq-net/verify/route-algebra/README.md)). The mark only turns on along
      a path, so extending a route still never improves it. A strictly
      cheaper marked route still wins; an operator who wants the unmarked
      side preferred raises its priority. On the bake-off it moved no
      settled result, only failure transients on peered tiers: 220 to 225
      for a home dying on 15 relays, 954 to 924 on 32 (rejected: leaving it
      to arrival order; dropping the next hop in use, which moves a
      subscription for an equal route; ranking the mark ahead of cost).
14. ✅ **A silent death is left to keep-alives and the opt-in metadata
    standby** ([route health](/quest/m3/route-health/README.md)). No arm
    fixed it: readers behind a relay that dies unnoticed for 3 s stay unfed
    until the last session notices, as today (rejected: `suspect`, ranking
    the far end of a closed session as draining until its own word arrives,
    which shortened some silent deaths, nearly doubled a flap from 656 to
    1,133, and left 20 to 42 suspicions that never lifted).

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit, which
judged the route layer at this plan's end state):

15. ✅ **The plan stands as item 2 has it** (rejected: freezing lite-07 with
    claims only and moving the route exchange behind a negotiated SETUP
    parameter with a version of its own, so the cut stops waiting on the
    router and the switch and a third-party client implements no route
    layer; keeping the plan with a fallback written down now). The audit's
    case was the schedule, three XL quests in series before the freeze on a
    rule re-decided on six days, and that an endpoint must parse a protocol
    only relays speak. What it left went into the switch, which
    [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) and
    [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053) finished:
    the public route API and config that would freeze by omission, the half
    `js/net` implements, and what a non-forwarding endpoint must parse. The
    moq-cluster draft specified the warm discount item 5 forbids until it
    was rewritten to what ships, since third-party relays ship it.

The shape those decisions give, as
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) built it; a
further departure is a decision to list in its PR:

- **Identity.** A path and an Instance name content, and a request names the
  Instance. A relay holds one upstream subscription per Instance and track,
  whichever routing node its downstream requests named, so a join stops at the
  first node already carrying it.
- **Reachability.** One route per announcing relay, offered once it has a
  claim: its path, drain, priority, and cost. A node never uses a route
  whose path names it, sends none to a neighbour already on the path, and
  refuses one at or below a session floor. Rank is not draining, then the
  lowest priority on the path, then the summed cost, each link adding at
  least 1, so extending a route never improves it. At equal rank a route
  with no marked link beats a marked one, then the next hop in use, the
  shorter path, and the neighbour (item 13). A closed session and a
  draining node are each named once per neighbour, and a closed session's
  message ends every route its sender offered through it. A node advertises
  once per burst of input.
- **Claims.** A prefix, Instance, Epoch, cost, the lowest link priority to
  its publisher, and whether the way there is draining, announced by a
  forwarding node for itself and its local endpoints, ordered by the node's
  revision and certified by floors as today, which measured no revival and
  no false END. A claims message carries its floor even with nothing
  changed, and a withdrawal says whether the publisher ended the claim or
  its announcer lost it.
- **Boundaries.** The chain of item 3.
- **Requests.** A request names the routing node its first hop chose, so
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

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the path rule this replaced, and the acceptance fixtures the cut still waits on
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - implements the warmth pick
- [Route health](/quest/m3/route-health/README.md) - the metadata-only secondary
