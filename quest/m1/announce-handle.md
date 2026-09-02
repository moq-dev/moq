# [L] Fold on-demand serving into the announce handle

## Goal

`origin::Producer` has one way to advertise and serve a prefix and one way to
publish a broadcast:

- `announce(prefix, route)` returns a handle that both advertises the route
  and queues the requests beneath it that the broadcast tree cannot resolve;
  the app accepts or rejects them. `dynamic()` is gone.
- `create_broadcast(path)` creates an unadvertised broadcast inside the
  origin, and `broadcast::Producer::set_announce(bool)` flips its exact-path
  advertisement once it is populated.

Every in-tree caller compiles against that surface and the Rust docs describe
only it. Bindings and js/net follow in their own quests.

## Plan

Why the broadcast is born inside the origin rather than attached later: a
track binds to the origin's cache pool when it is created
(`cache::Track::new(broadcast.origin.pool, ..)` in
`rs/moq-net/src/model/track.rs`), so a standalone broadcast populated before
attach would keep the unbounded default pool. Create, populate, then announce
is the order, and the boolean is what makes the last step one call.

Model changes in `rs/moq-net/src/model/origin.rs` on `dev`:

- `announce(prefix, route) -> Announce` is today's crate-private
  `announce_served` made public: the advert guard plus the `RouteServer`
  request queue, merged into one handle. `Announce::update(route)` stays;
  `poll_requested(waiter)` and an async `requested()` yield `Request`
  (`path`, `accept`, `reject`). The queue is unbounded and drains only when
  the handle drops, which retracts the advert and rejects what is queued.
  Sessions use the same handle for the routes a peer announces to them.
- `dynamic()` and `origin::Dynamic` are deleted. A caller that served a root
  fallback announces `""` instead; on `dev` the relay already announces
  routes and only moq-mux tests still use `dynamic()`.
- A request resolves against the tree first, then the covering announce
  chosen by the route ranking #3225 defined (hops, cost); its handle gets the
  request.
- `create_broadcast(path)` keeps its signature and stays unadvertised.
  `broadcast::Producer::set_announce(bool)` (default `false`) creates or drops
  an exact-path advert that the origin owns alongside the tree entry, so the
  advert retracts with the broadcast on finish or drop.
- `broadcast::Producer::dynamic()` (tracks on demand within a broadcast) is
  untouched; the catalog is a track's announcement.

In-tree migration in the same PR, since the change is breaking: the
create-then-announce pairs in `moq-cli`, `moq-bench`, `moq-boy`, `moq-hls`,
`moq-rtc`, `moq-transcode`, `moq-gst`, and `hang/examples` become
`create_broadcast` + `set_announce(true)`; `moq-relay` keeps
`announce(prefix, route)` for cluster and node routes; moq-mux tests move
from `dynamic()` to an announce handle. `moq-ffi` and `libmoq` get the minimal
internal change to keep compiling (their `MoqOriginDynamic` reads the announce
handle's queue); their public surface changes in the bindings quest.

Docs: `doc/lib/rs/env/tokio.md`, `doc/lib/rs/crate/moq-net.md`, and the
`doc/concept` pages that mention dynamic serving describe only the new shape.

Tests: an announce handle serves a request under its prefix; dropping an
unpolled handle rejects its queued requests; `set_announce` toggles what
`announced()` reports and retracts on finish; a consumer that subscribes on
the first announcement finds the populated tracks; two handles covering one
path rank by route.

Branch from `dev`.

## Related

- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - the bindings adopt this surface
- [JS announce](/quest/m1/js-announce.md) - js/net mirrors it
- [#2895](/quest/m1/2895-add-an-atomic-readiness-gate-for-origin-broadcasts.md) - readiness gating on the same producer
