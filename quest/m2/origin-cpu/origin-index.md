# [L] Index the origin route table

## Goal

`best_route`, announce cursor replay, and `request_broadcast` are logarithmic
(or otherwise sublinear) in the number of live advertisements. Hop/cost/
split-horizon ranking does not change.

## Plan

`OriginState::routes` is a `Vec<RouteEntry>` with the comment "Scans are
linear: the table holds one entry per live advertisement."
`best_route` collects every covering entry into a `Vec` then takes longest
prefix plus `route_order`. Every announce runs `sync_route` across all
cursors, and `sync_cursor` re-filters the entire list.

Keep one source of truth, but index by path prefix (radix / segment tree /
`BTreeMap` of prefixes with a covering-chain walk). `best_route` walks the
path's prefix chain. `sync_route` recomputes only cursors whose scopes
intersect the changed prefix. Keep `route_order` (cost, hop length, FNV,
newest id). Do not shard the origin across workers (out of scope in
[perf](/quest/m1/perf/README.md)).

Fold the empty `HashSet::new()` on the miss path (`request_broadcast`) into
the same signature change: `best_route` takes `Option<&HashSet<u64>>` or a
static empty set.

Acceptance: new Criterion target `rs/moq-net/benches/origin.rs`: announce N
prefixes, then `best_route` / `request_broadcast` / cursor replay; sweep
N = 1k/10k/100k, cursors = 1/8/64. Plus `just bench` with
`rs/moq-bench/config/announce.toml`. Lookup and announce CPU flat in N (or
log N). Hop/cost/split-horizon tests in `origin.rs` still pass. Zero alloc
on the common "no refused ids" lookup.

## Related

- [Relay memory](/quest/m2/relay-memory.md) - bytes per announcement, not this CPU
- [Route gauge](/quest/m2/route-gauge.md) - operator visibility
- [Origin local tree](/quest/m2/origin-cpu/origin-tree.md) - the other origin structure
