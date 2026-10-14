# [XS] Bench JS request cancellation across competing routes

## Goal

`js/net/bench/requests.ts` sweeps request cancellation over the number of
`dynamic()` routes competing for one path and the number of cancelled
requests, so a per-cancel cost in `OriginState.forget` that grows with the
routes shows up as a slope.

## Plan

#5411 made cancellation release origin demand. Codex flagged on that PR that
`forget` walks every route and failover slot on each cancellation; the merge
argued the walk only covers the cancelled path's own prefixes, the same
entries `bestEntry` already scans. Both cancellation sweeps in the bench use a
single route today, so neither claim is measured.

Add the two-axis sweep (for example 1/16/128 routes on one prefix by
1/64/1024 cancellations). If it shows a slope, fix `forget` in the same PR,
starting with computing the failover-entry set once instead of copying it in
the inner loop.

Public API: none. Wire: none.

## Related

- [JS request deadline](/quest/m1/js-request-deadline.md) - every timed-out request releases its demand through the same path
