# [M] A route change wakes only the fronts it affects

## Goal

A route change under a prefix (join, withdraw, re-price, restale) makes the
origin driver act only on the fronts that could change their selection:
fronts still waiting for a source, fronts the changed route serves or is
requesting through, and, on a join or re-price, the serving fronts the
changed route could now win. Today it wakes every front below the prefix,
so one equal-cost pool member joining or leaving a prefix that serves 10k
paths costs the driver 100 to 600 ms of single-threaded work while few
fronts switch (`origin/pool_churn`, #4607). A root claim, such as an
archive's catch-all, makes every front on the relay pay for every change to
it. Done when a leave in `origin/pool_churn` is flat in served paths, and a
join or re-price costs one cheap rehash per front below the prefix, with
no `select`, track poll, or deadline scan for a front that keeps its route.

## Plan

Facts from `main` (`rs/moq-net/src/model/origin.rs`, since Wildcard landed in
#4403):

- Every route change calls `sync_route`, which calls
  `routes.poke_below(prefix)`. That bumps a counter-only `Watch` on every path
  below the prefix, so a woken front cannot tell what changed. Each re-runs
  `select` under the table read lock (`retain_routes`, then `best_route`,
  which hashes every pool member), then the driver polls every track and
  rescans deadlines. `Pin` is gone (#4741).
- `route_order` ranks the most specific claim first, then drain and
  availability, priority, and cost, and breaks the remaining tie on lite-07
  by the draft's Rendezvous Hash of the path and the claim's Instance
  (`rendezvous`), lowest first. A join moves only paths the newcomer wins, a
  leave only paths the leaver was winning.
- On lite-07 the route layer writes into this table through `route::node`,
  one write per broadcast whose way changed; a change to one route rewrites
  every broadcast behind it, which [route benches](/quest/m3/route-benches.md)
  owns. This quest is the fronts' half: what each write wakes.

Decisions:

- Covers every route change, not only equal-cost pools. The pool is where
  the benchmark found it, but the wake is per prefix regardless of cost.
  (2026-09-30)
- An index, not a payload filter on the watch: each route records the
  fronts it serves or is requesting through, and each prefix node records
  only its waiting fronts and the pending `request` and `follow` resolutions
  that retry when their covering routes change (`routed_broadcast` became
  these in #7172). A payload filter
  would still walk every watch under the prefix, which stays linear in paths.
  (2026-09-30)
- No pin sets (decided 2026-10-03: #4741 deletes `Pin`, so any covering route
  qualifies for a waiting front). Any change to a route wakes the waiting set
  below it and the fronts it serves or is requesting through. Waiters wake on
  a loss too: withdrawing a deeper claim can unshadow a broader route for a
  parked waiter it never served. A withdrawal takes the served set before
  dropping the record. A front moves its entries when it selects and drops
  them when it closes. The lookup walks descendants but skips subtrees whose
  waiting set is empty, keeping cost in woken fronts rather than served
  paths.
- A serving front follows the best route among those it may resume across
  (decided 2026-10-04, replacing Stay; narrowed 2026-10-07 after #4942 and
  Restart; restated for lite-07's Instance): claims of its Instance, at the
  newest Epoch among them. A front whose Instance another Broadcast replaced
  at the path stays on its claims for its subscribers until they go and never
  splices onto another Instance (maintainer, 2026-10-08); new requests take a
  fresh front on the winner instead of waking it. An idle front (every track
  forgotten, no consumer holding its broadcast, not waiting for coverage)
  ends instead of moving. A join or re-price must rehash every front below
  the prefix that may resume, since rendezvous moves exactly the paths the
  changed route now wins; only those fronts re-select. A leave wakes only
  the fronts the leaver served or was requesting through.
- Builds on fronts keyed by effective exclusion, so the index hangs off the final front
  identity. (2026-09-30)

Decided 2026-10-13: coordinate the index with the route model and per-track
active/standby selection. Do not build a second route table per track or
turn one route's cost change into per-broadcast mutations. Whichever
implementation lands first, this or the origin record in
[route benches](/quest/m3/route-benches.md), preserves these operation-count
tests for the next.

Verification: `origin/pool_churn` leaves flat in served paths at every pool
width it already sweeps, and a unit test that counts `select` calls per
route change: zero for an unrelated route's leave, only the won paths on a
join, and a parked waiter retrying when a deeper advertise-only claim over a
served root is withdrawn.

Public API: none. Wire: none.

## Required

- [Cache-aware routing](/quest/m3/cache-routing/README.md) - adds the warm pick to how a relay chooses its source, so the index is built once on the final selection

## Related

- [Route benches](/quest/m3/route-benches.md) - the route layer's side: one origin record per route instead of a write per broadcast
