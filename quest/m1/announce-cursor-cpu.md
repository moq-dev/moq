# [M] Announce cursors stay linear under churn

## Goal

An announce cursor's work per event is independent of how many entries it
holds back. When an origin holding K broadcasts goes away, each cursor does
O(K log K) work, not O(K²). A route change that leaves a cursor's winning
route unchanged allocates nothing for that cursor. Both properties have an
operation-count regression test and a benchmark, and the fix is backported
to `release`.

## Plan

Found in the 2026-10-22 clustering audit; both costs multiply by the number of
peer cursors, which on a dense mesh is about 20 per relay.

- `scan` in `rs/moq-net/src/model/origin.rs` walks the whole `pending` map
  from the front on every `poll_next`/`try_next`. Held and quarantined entries
  stay at the front and are re-walked for every ready entry behind them. Each
  new hold also schedules a wake 1 ns later. Replace the walk with a deadline
  heap for held entries plus a ready queue, so a poll touches only what is
  due. Keep the 300 ms update hold and the 2 s retraction quarantine
  semantics unchanged; [Routes and announces](/quest/m1/cluster-routing/routes.md)
  retires them, not this quest.
- `sync_route` calls `TableCursor::update` for every visible cursor on every
  route change at a prefix. `update` clones the hops, epoch, and auth and
  rebuilds the captures before comparing against the previous value, so most
  calls allocate only to find a no-op. Compare the new winner against the
  cursor's current entry by reference first (instance, captures,
  servability, and every metadata field `update` delivers: hops, cost,
  entry, epoch, auth) and clone only on a difference. An in-place update
  that keeps the route and instance but changes metadata must still reach
  the cursor.

Tests: count `scan` steps and `update` allocations per event, not
wall-clock time. An origin dropping K broadcasts costs each cursor at most
O(K log K) steps, swept over K. A route arriving that does not change a
cursor's winner allocates nothing for that cursor. Keep the `mesh_withdraw`
residual fixtures passing with unchanged event sequences, since this changes
cost, not behavior.

Benchmark in `rs/moq-net/benches/announce.rs` (or `origin.rs`), swept over
both fan-out axes: one origin withdrawing 1k and 10k broadcasts, and one
broadcast's route churning, each under 1, 20, and 100 cursors, against `main`
before the change.

Backport: per CONTRIBUTING, land on `main`, then port onto `release` as a
separate PR keeping the published API. No wire change, no public API change.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the real fix for per-broadcast route churn; this only makes today's cost linear
- [Route wakes](/quest/m2/route-wakes.md) - the same kind of fix for origin fronts, not announce cursors
