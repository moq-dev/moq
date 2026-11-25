# [L] A route arrival re-ranks only what it changes

## Goal

A route arriving at, changing at, or leaving a path costs each reader of that
path work that does not grow with the other routes already there (a log factor
at most), so a burst of N claims at one path is linear in N per reader, not
quadratic. This covers every selector an arrival runs, in both languages: the
lite-07 claims reader and the JS consume cache in `js/net`, and the origin's
announce cursors in `moq-net`. What each reader is told, and in what order,
stays exactly as today.

Done when `js/net/bench/routed-claims.ts` (instances at one path × readers)
settles in time linear in instances at each reader count, and Rust's
`origin/announce_duplicate` (routes at one path × cursors) is flat in routes at
each cursor count, both swept to at least 1024 routes.

## Plan

Found by [moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371), which
adds the JS bench. Facts, from `main` at e56d723e60 and that PR's head:

- JS `Routed` (`js/net/src/lite/routed.ts`): each arrival runs `#touch`, which
  for every reader rebuilds every candidate at the path (and at every covering
  filing for a reader rooted below it), re-picks each instance with `claims`,
  or the one best without, and diffs every instance against `reported`. On
  `main`, 1024 instances settle in 82 to 84 ms with 1 reader and 2.7 to 2.9 s
  with 32; on that PR's head, 129 to 157 ms and 3.6 to 6.0 s, the extra being its
  cross-filing ranking.
- JS `BroadcastCache` (`js/net/src/consume.ts`): each `announce` and `withdraw`
  re-runs `instance(path)`, a full `mostSpecific`, for every consumed path
  beneath the prefix.
- Rust `sync_route` (`rs/moq-net/src/model/origin.rs`): each add, update, or
  removal sorts every entry at the prefix by `route_order`, then each touching
  cursor rescans that list with `Epochs::of`; a cursor's root row runs
  `TableCursor::best` over every covering entry. Measured here
  (`cargo bench -p moq-net --features fuzz --bench origin -- origin/announce_duplicate`),
  one arrival costs 3.1 µs at 1 route and 1 cursor, 4.7 µs at 30 and 1,
  143 µs at 60 and 60, 286 µs at 240 and 60, and 1.12 ms at 240 and 240.

Recommendations:

- With `claims`, an arrival or end of one instance re-picks and diffs only that
  instance's key; the others' picks cannot change.
- For a single best (JS without `claims`, the consume cache, Rust cursors),
  keep each filing's routes in rank order, updated per change, so an arrival
  compares against the winner and the winner leaving takes the next. Epoch
  supersession and per-cursor visibility (split horizon, exclusions) skip
  entries from the front, which a reader pays only for the routes it cannot
  see. A root row's hash keys on the cursor's root, not the filing, so it
  needs its own order or a bounded fallback; measure before choosing.
- The tie-breaks that depend on arrival (newest wins, Rust's `since`, JS
  `compareTie`) must come out the same. Keep today's full pick as a test oracle
  and check it against the incremental one over random sequences of arrivals,
  re-prices, epoch restarts, drains, and ends, in both languages.
- Out of scope: waking and re-selecting fronts below the prefix
  (`poke_below`, `best_route`), which
  [route-wakes](/quest/m3/route-wakes.md) owns.

Public API and wire: none expected.

## Related

- [Route wakes](/quest/m3/route-wakes.md) - the fronts' half of a route change's cost
- [Mesh session cost](/quest/m9/perf/mesh-session-cost.md) - per-session cost in the mesh tests, `sync_route` among it
