# [L] Announce to serve

## Goal

A broadcast exists for other people only while it is announced, locally and
remotely alike. Until `announce()`, a created broadcast is invisible to every
announce cursor and `request_broadcast` refuses it with `Unroutable`.
`unannounce()` withdraws it from local consumers as well as peers, and a later
`announce()` brings it back. A local broadcast competes on its announced cost
like any other route, so a cheaper remote route wins over it. A consumer in the
same process sees exactly what a consumer across a session sees; only the
latency differs.

## Plan

The maintainer calls today's behavior a bug: creating a broadcast and expecting
anyone to see it without announcing it. So this is a fix and targets `main`.

It reverses two earlier choices:

- #3928 (the local-announcements quest, #3921) put every created broadcast on
  local announce cursors from creation, reasoning that a relay's own ingest is
  what a local consumer wants to find. That ingest should announce instead.
- Before that, `create_broadcast` already resolved by exact path without an
  announcement, so "cached or on-demand content can stay reachable without
  ever being announced". On-demand serving already has an announced form in
  `origin::Producer::dynamic`, which claims a prefix. Move each such caller to
  it or to `announce()`.

In `rs/moq-net/src/model/origin.rs`, a route entry with `advertised: false`
becomes invisible to every cursor (drop the `entry.local && exclude.is_none()`
arm in `TableCursor::visible`) and unservable by `best_route`. The
`AnnounceProducer::withdraw` path then makes `unannounce()` a full retraction.
Retraction ends a front the way #4007 made it: in-flight tracks carry on to
their own FIN or reset.

A front ends when its route leaves the table and no route with the same
content identity replaces it, even if its source is still alive. Today
`Front::selected` keeps serving a live source with no route ("its route may
return"), and `request_broadcast` joins a front whose route has retracted. So
after a local `unannounce()` the broadcast stays open and requestable. Remotely,
ANNOUNCE_END ends the source the subscriber minted, so the front closes and new
requests get `Unroutable`. A re-announce that lands before the front acts on
the retraction reuses the same route entry, so the front carries on: the same
window a remote reannouncement gets.

Route selection stops preferring local entries. `OriginState::best_route` and
the announce cursor's pick in `OriginState::sync_route` both order candidates by
`(!entry.local, route_order(..))`, so a local route wins whatever its cost. Drop
the `!entry.local` key. Cost then decides, and a tie still falls to the local
route because it has no hops. This decides which route a front starts from;
once it has a source, its identity pin (`Pin::Local` or `Pin::Publisher`) still
limits failover to the same content.

Audit every caller that creates a broadcast and relies on it being visible or
requestable unannounced. As starting points, these files create one with no
`announce()` call in the same file:

- `rs/libmoq/src/origin.rs`
- `rs/moq-ffi/src/origin.rs`
- `rs/moq-hls/src/export/rendition.rs` and `rs/moq-hls/src/export/upstream.rs`
- `rs/moq-mux/src/catalog/hang/consumer.rs`
- `rs/moq-net/src/lite/subscriber.rs`

Revert the prose #3928 changed: the `create_broadcast` and `announce` docs in
Rust and `js/net`, and the binding docs and `doc/lib/*` pages. JS mirrors
Rust: `js/net/src/origin.ts` got the same local visibility in #3928.

Test each scenario twice, once through a consumer of the same origin and once
across an in-memory mock session (`rs/moq-net/tests/support/harness.rs`), and
assert both observe the same thing:

- created, not yet announced
- announced
- unannounced with a track in flight: the track reaches its own end
- unannounced with a track in flight, then a fresh request for the path: it
  answers `Unroutable` rather than joining the broadcast still draining
- announced again

## Related

- [Broadcast close](/quest/m1/broadcast-close/README.md) - `close()` is a permanent `unannounce()`, and every broadcast end must behave the same locally and remotely
- [Hidden broadcasts](/quest/m1/hidden-broadcasts.md) - the other rule for which announced paths a cursor sees
