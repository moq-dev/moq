# [M] A route change wakes only the fronts it affects

## Goal

A route change under a prefix (join, withdraw, re-price, restale) makes the
origin driver act only on the fronts that could change their selection:
fronts still waiting for a source, fronts served by the changed route, and
publisher-pinned fronts on its first hop. Today it wakes every front below
the prefix, so one equal-cost pool member joining or leaving a prefix that
serves 10k paths costs the driver 100 to 600 ms of single-threaded work
while no front switches (`origin/pool_churn`, #4607). A root claim, such as
an archive's catch-all, makes every front on the relay pay for every change
to it. Done when `origin/pool_churn` is flat in served paths.

## Plan

Facts from the wildcard line (`rs/moq-net/src/model/origin.rs` there):

- Every route change calls `sync_route`, which calls
  `routes.poke_below(prefix)`. That bumps a counter-only `Watch` on every path
  below the prefix, so a woken front cannot tell what changed. Each re-runs
  `select` under the table read lock (`retain_routes`, then `best_route`,
  which hashes every pool member), then the driver polls every track and
  rescans deadlines.
- `route_order` is rendezvous-style: FNV over the path and hop ids, lowest
  wins. A join moves only paths the newcomer wins, a leave only paths the
  leaver was winning.
- A serving front is `Pin::Stay` and ignores joins and re-prices; it moves
  only when its own route is withdrawn or its source closes. So on a join
  the fronts that can act are the waiting ones (no live source, including an
  upstream request in flight), publisher-pinned ones on the new route's first
  hop, and local-pinned ones for a local route. On a leave, the fronts served
  by or requesting through that route. A deeper prefix appearing shadows
  shallower routes for the same non-Stay fronts.

Decisions:

- Covers every route change, not only equal-cost pools. The pool is where
  the benchmark found it, but the wake is per prefix regardless of cost.
  (2026-09-30)
- An index, not a payload filter on the watch: each route records the
  fronts it serves, and each prefix node records only its non-Stay watches
  (waiting fronts, publisher- and local-pinned fronts, `routed_broadcast`
  waiters). A change wakes those plus the changed route's own fronts. A
  payload filter would still walk every watch under the prefix, which stays
  linear in paths. (2026-09-30)
- Builds on shared-fronts' keying, so the index hangs off the final front
  identity. `origin-front-parks.md` replaces the `routed_broadcast` retry
  loop, one of the watch consumers here; whichever lands second adapts it.
  (2026-09-30)
- The wildcard line's `quest/m1/front-upgrade.md` will make a cheaper route
  matter to Stay fronts, roughly those the new route would win. Keep the
  index shaped so that set can be added without walking every served path.

Verification: `origin/pool_churn` flat in served paths, and a unit test
that counts `select` calls per route change and asserts only affected
fronts re-select. Keep `pool_resolve` unchanged.

Public API: none. Wire: none.

## Required

- [Viewer sessions share a front](/quest/m0/shared-fronts.md) - re-keys fronts, which this index hangs off; it lands after the wildcard line, which is where the pool code lives

## Related

- [Front deadlines](/quest/m1/front-deadline-index.md) - each spurious wake also pays that per-track poll and deadline scan
- [Front parking](/quest/m1/origin-front-parks.md) - replaces the `routed_broadcast` watch loop this wakes
