# [S] Idle GC

## Goal

`track::Pool::advance` with nothing due does no clock read, lock, or waker
registration. `track_gc/idle` returns to a few nanoseconds.

## Plan

Found by reading code on 2026-10-21 (11.6 ns to 74 ns, flat across sizes,
confirmed). [#5548](https://github.com/moq-dev/moq/pull/5548) (wall-clock
aging of superseded groups) added the following to every `advance` in
`rs/moq-net/src/model/cache.rs`:

- the `wall_epoch` `OnceLock` and a `wall_micros.fetch_max`;
- `release_due(self.now_wall(), driver)`, which reads `moq_clock::now()`;
- the `inner.wall` lock and `driver.register`.

Part of the cost is the bench itself: it builds a fresh `kio::Waiter::noop()`
on every iteration, so each `register` allocates and frees an `Arc<Waker>`. A
real driver reuses its waiter. Hoist the waiter out of `b.iter` first, and
measure what remains.

Fix: keep the earliest wall deadline in an atomic, as `expiry.rs` already
does with `earliest`. Read `now_wall()` and take the lock only when that
deadline has passed. Time decisions use `max(wall, pts)`; keep that behavior.
