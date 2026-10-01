# Cluster routing

## Goal

A cluster carries each broadcast into a region once and never exposes its
backbone to end users. Relays take an explicit edge or core role: edges
serve end users and spread paths over their region's cores, cores link to
the cores of other regions over configured links with static costs, and
routing between them stays path vector. A broadcast under overlapping
prefixes routes to one origin deterministically. Redundancy an operator
configures may deliver more than one copy.

Non-goals: edges acting as each other's intermediates (it makes edges a
bigger DDoS target), cache-aware route switching, and a permanently
mixed-version cluster.

## Plan

### Decisions

Settled in the 2026-09-30 wildcard audit (cache tiers), replacing the
link-state and existence-split design planned earlier that day:

- Two roles, edge and core. Where a region has one edge, that edge is also
  its core. A region may have several cores, which do not link to each other.
  An edge dials every core in its region over qmux on TLS (intra-region links
  are not congested) and picks one per path by rendezvous hashing, so a
  broadcast crosses into a region once. An edge never re-advertises one
  core's routes to another, so it is never transit. Cores dial the cores of
  the regions they link to, only those with lower names, so each pair has one
  connection. Cores are hidden from end users, which is the DDoS shield. See
  [Edge and core](/quest/m1/cluster-routing/roles.md).
- Core links are configured and static, and may skip PoPs when the RTT is
  short: Seattle pulls from San Jose directly rather than via Oregon. Nothing
  switches routes on cache state, so Warm and Cold collapse to one route cost
  ([One route cost](/quest/m1/route-cost.md)), and the wildcard line's route
  upgrade quest is deleted.
- No link-state topology. With split-horizon edges and a sparse core graph,
  path vector with hop lists stays loop-free, path hunting is confined to the
  cores, liveness flooding (the simulator's dominant link-state cost) is never
  paid, and failure detection sets the outage window either way. The parked
  implementation is #4631. Stale paths after a withdrawal are ended by
  per-origin seqnos on cluster links
  ([Path hunting](/quest/m1/cluster-routing/path-hunting.md)); a lite-06
  hold-down (#4644) was rejected because it breaks seamless failover, and
  #4642's cursor hold mitigates production meanwhile.
- The line lands on `main`: its children are additive. The two breaking
  changes left it for `dev` on their own:
  [Remove `--hop`](/quest/m1/hop-removal.md) and
  [One route cost](/quest/m1/route-cost.md).
- Cluster links are moq-lite only; moq-transport peers are plain clients
  ([moq-transport peers are plain clients](/quest/m1/ietf-cluster-off.md)).
- An epoch-qualified concrete path (`foo/@<uuidv7>`) is a source's identity:
  origins that announce the same one are interchangeable, which is how a
  redundant pair is expressed. A path a claim produces keeps Wildcard's
  per-origin identity. See [Selection](/quest/m1/cluster-routing/selection.md).
- Between clusters, announcements stay path vector with cluster ids as hops,
  in the last child.
- Anything specific to moq.pro's deployment (generating peer lists and roles
  from its inventory, its simulator) is planned in moq.pro, not here.

### Simulator findings

Measured on the earlier flat mesh; they motivated the link-state design the
tiers decision replaced, and still bound what path vector costs.

The report is moq.pro's `just rs sim` (every scenario on live's graph) and
`just rs sim sweep` (synthetic regional graphs of 34, 340, and 1020 relays).
It carries messages and bytes by kind, per relay and cross-region,
convergence, loop, stall, and failover windows, and state per relay and per
registry.

- Existence costs about one message per relay per event flooded, and one per
  interested relay plus one per remote registry with registries. On live, a
  wide customer prefix with `.stats` from every relay cost 2.5M announces
  today, 4k flooded, and 3.5k with registries, whose relays held 63 records
  on average against 94.
- Liveness is the split's dominant cost at scale: flooding every link repeats
  each report once per neighbour. At 1020 relays (mean degree 35), a link
  loss and restore plus a relay loss and restart cost about 290 MiB of
  liveness flooding and 54 MiB of session-up digests against under 1 MiB of
  existence for twenty publishes, while a publish costs exactly one message
  per relay.
- Unbatched, one relay restart at 340 relays sent half a million liveness
  messages; batched for 50 ms, 27k.
- Failure detection, not routing, sets every outage window: a silent link or
  relay loss is noticed after the 30 s QUIC idle timeout in every candidate,
  and subscribes through it go nowhere until then.
- The simulator saw no loop while views agreed, and HRW split an equal-cost
  pool 63/49 where a hash of the announced prefix sent all of it to one
  sibling (Wildcard has since keyed the tie on the requested path). On live's graph no disagreement looped across 20 seeds of link,
  cost, and relay churn.
- One registry per region and two cost about the same; two halves the
  busiest registry's load. A registration sent on a dead registry session
  that nobody has noticed yet waits for the failover.
- [#4399](https://github.com/moq-dev/moq/pull/4399) trades path hunting for
  flapping. In its own full mesh of 8 it retracts each of 100 withdrawn
  broadcasts exactly once per relay, as its tests show. On live's graph, with
  each writer coalescing for 1 ms, ending three paths loops subscribes for
  9 s instead of 41 s and edges report the ended paths for 38 s summed
  instead of 93, but it costs about the same 60k messages, and clients see
  17k updates instead of 12k, with ended paths re-announced 4185 times
  instead of 163: each re-announce by a peer revives every route through it,
  stale ones included. Written without coalescing, the revivals cascade, and
  the same scenario costs 18.6M messages instead of 189k. Per-origin seqnos,
  above, end both.

### Remaining work

Once every child has landed:

- An end-to-end test: a two-region cluster in `rs/moq-relay/tests` where a
  publish, an end, a core loss, and a redundant-pair failover each reach a
  subscriber on a far edge, with time mocked.
- Rewrite `doc/bin/relay/cluster.md` into the operator's view (edge and core
  layout, dial rules, TLS edge links, link costs, idle timeout, redundant
  pairs), and add a routing page under `doc/concept`.

## Required

- [Edge and core](/quest/m1/cluster-routing/roles.md) - relays take an explicit edge or core role; edges spread paths over their region's cores and are never transit
- [Selection](/quest/m1/cluster-routing/selection.md) - a broadcast under overlapping prefixes routes to one origin deterministically, and same-epoch origins are one source
- [Path hunting](/quest/m1/cluster-routing/path-hunting.md) - per-origin seqnos on cluster links end stale re-announces without breaking seamless failover
- [Between clusters](/quest/m1/cluster-routing/inter-cluster.md) - announcements crossing a cluster boundary stay path vector with cluster ids as hops

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - the longest-prefix rule, pool spread, and reply identity Selection builds on
- [Remove `--hop`](/quest/m1/hop-removal.md) - on `dev`: redundant publishers share an explicit `@<epoch>`
- [One route cost](/quest/m1/route-cost.md) - on `dev`: Warm and Cold collapse to one static cost
- [Same-hop importers](/quest/m1/hop-aligned-import.md) - the importer half of a redundant pair; `--hop` removal re-keys it to a shared epoch
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - a redundant pair shares one epoch
- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - its #4349 report shows closed broadcasts announced for up to 229 s, evidence for per-origin seqnos
- [Routing cost domains](/quest/m3/routing-cost-domains.md) - cost across the cluster boundaries this keeps path vector
