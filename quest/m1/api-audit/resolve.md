# [L] Two broadcast resolutions

## Goal

An application resolves a broadcast in one of two ways, named the same in
Rust, JS, and every binding:

- `request(path, epoch)` resolves one publisher instance. It owns the
  discovery interest it needs, so on a watch-driven lite client it finds a
  broadcast the peer serves without the caller watching the path first, and
  it retries across route churn. It answers `Unroutable` only once discovery
  has synced with no covering route.
- `follow(path, epoch)` follows the path across publisher instances (today's
  `announced_broadcast`), pinned to `epoch` when given.

No caller can assemble a racy two-step resolve, and no crate hand-rolls
"resolve once discovery synced".

## Plan

Decided 2026-10-22 (API audit, maintainer approved the recommendation).

Rust `origin::Consumer` today has five overlapping operations
(`rs/moq-net/src/model/origin.rs`): `routed(path) -> Option<Route>` (swallows
errors), `follow(path) -> Follow` (route level), `routed_broadcast(path)` (no
epoch), `announced_broadcast(path, epoch)`, and `request_broadcast(path,
epoch)`. Per `rs/moq-net/MIGRATE.md`, `request_broadcast` on a client fails
`Unroutable` for a path no reader watches, even when the peer announces it,
and the bindings promote it as a top-level convenience.

- Rust: `request(path, epoch) -> Pending<Requesting>` takes
  `routed_broadcast`'s semantics plus the epoch pin and the synced
  `Unroutable` answer, with no wall clock (relays never block on peers).
  `follow(path, epoch)` is today's `announced_broadcast`. Delete `routed`,
  `routed_broadcast`, and `Requesting::is_queued`. The immediate table lookup
  (today's `request_broadcast`) and the route-level `Follow` become
  `pub(crate)` for the sessions; route inspection stays where a consumer
  needs it (the `routed()` callers in moq-cli reporting move to whatever
  route observation remains public, or drop the report).
- JS (`js/net/src/origin.ts` `Consumer.request(path, {announced, epoch})`):
  two methods, `request(path, {epoch})` and `follow(path, {epoch})`. Fix the
  mismatch where `{announced: true, epoch}` closes when the instance ends
  while Rust's pinned follow hands back the same instance if it returns. Drop
  the silent fallback that turns `announced: true` into a blind request when
  discovery is unavailable. `Producer.request`/`announced` pass-throughs stay
  only if `Table` needs them. moq.pro's five `{announced: true}` sites move
  to `follow`.
- Bindings: `request_broadcast(path, epoch)` and `announced_broadcast(path,
  epoch)` map onto the two operations under the same names as Rust, and the
  Kotlin, Dart, Go, and Python top-level shortcuts follow.
- Callers to fix: `rs/moq-wasm/src/lib.rs` (`routed()` then
  `request_broadcast()`, error hidden by `.ok()`), `rs/moq-srt/src/ts.rs`
  `Subscriber::new` (production), `rs/moq-cli/src/subscribe.rs` and
  `report.rs`, `rs/moq-transcode/examples/transcode.rs`,
  `rs/moq-bench/src/connection.rs`, the hand-rolled resolvers in
  `rs/moq-mux/src/source.rs`, `rs/moq-hls/src/server/mod.rs` (wall-clock
  `RESOLVE_TIMEOUT`) and `rs/moq-stats/src/aggregate.rs`, the
  `rs/moq-relay/src/web.rs` fetch deadline, the `rs/moq-e2ee/MIGRATE.md`
  advice to read the epoch via `routed()`, and
  `demo/carla/sim/moq_carla_sim/publish.py`, which reads the epoch from an
  announce and then requests unpinned.

The prior audit's race claim is narrower than stated: discovery lingers 2 s
after `routed()` drops its follower, so the window is a route retraction or
failover between the two calls.

Tests: a lite client resolves an announced but unwatched path; a pinned
`follow` hands back the same instance when its route returns, in Rust and JS.

Public API: breaking in moq-net, `@moq/net`, moq-ffi, and every wrapper.
Wire: none.

## Related

- [Readers follow restarts](/quest/m1/sink-restart-follow.md) - moves readers to `follow`; whichever lands second adopts the other's names
- [Reference announcements](/quest/m0/broadcast-epoch/reference-announcements.md) - `moq_mux::Source::follow` builds on the follow operation
- [Origin fronts park](/quest/m2/origin-front-parks.md) - touches the same retry
