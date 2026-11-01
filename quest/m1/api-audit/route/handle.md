# [XL] One Route handle

## Goal

On the publishing side, ownership runs origin → route → broadcast → track →
group → frame. An application creates a `Route` on the origin, and the
route creates and owns its broadcast. `Publication`, `Prepared`,
`Announcer`, `Requests` and `Producer::publish` are gone, in Rust, JS,
moq-ffi, moq-c and the wrappers:

```rust
// An exact route owns one broadcast; dropping the route withdraws it.
let route = origin.route("live/alice", Options::hidden())?;
route.broadcast().create_track("catalog", info)?;
route.announce();  // visible now, same identity
route.set_cost(5); // re-prices in place
route.withdraw();  // hidden; announce() again continues the same stream

// A prefix route serves requests.
let claim = origin.route_prefix("transcode", Options::default())?;
while let Some(request) = claim.requested().await {
    // Either serve a broadcast already held, under the claim's identity...
    // request.serve(held);
    // ...or create the path's own exact route.
    let job = request.route(Options::default())?;
    spawn(run(job));
}
```

A `Route` mints its identity when it is created and keeps it for its
lifetime, across withdraw, re-announce and reconnects, because it owns the
same broadcast and its sequences continue. A new run is a new `Route`. Apps
pass only options: visibility at creation, `cost`, `auth`, and a stable
`identity` for a publisher that keeps its sequence space across restarts
(see [Identity and epoch](/quest/m1/api-audit/route/identity.md)). Setting
an identity grants no more than a minted one does: publishing still
requires the path's grant. A route exposes its own identity (`route.identity()`), so the OBS dock can
show it again. Hops, via, source and the anonymous mark become
read-only facts on a received announcement, and `Hop`, `Hops` and
`HopLimit` leave the public API.

Non-goals: the identity model ([Identity and epoch](/quest/m1/api-audit/route/identity.md)),
announce events ([Listing and follow](/quest/m1/api-audit/route/events.md)),
the read side, which identity owns, and the route layer's wire metric
([Routes and announces](/quest/m1/cluster-routing/routes.md)).

## Plan

Decided 2026-10-30 (maintainer); see the [epic](/quest/m1/api-audit/route/README.md).
The names above are a sketch. Settle the final ones in the PR, keeping Rust,
JS and the bindings identical.

- **The route owns the broadcast.** `route.broadcast()` returns the
  `broadcast::Producer` that the 18 library entry points already take
  (`moq-mux` catalog and importers, `moq-hls` import, the `moq-rtc` codecs,
  `moq-room` chat, `moq-boy` status). Today the broadcast holds its own
  announcer (`rs/moq-net/src/model/broadcast.rs:252-292`, and
  `js/net/src/broadcast.ts:391`). That inversion goes: the broadcast knows
  nothing of routes, and `broadcast::Producer::announce`/`unannounce`/
  `route` are removed. A standalone `broadcast::Producer` stays for
  in-process pipelines and tests. It can only be published through a
  prefix route's `serve`, as today, where a standalone broadcast cannot be
  announced.
- **Prefix routes.** A prefix route yields requests, and each one is
  answered by `serve(consumer)` (from [serve](/quest/m1/api-audit/route/serve.md)),
  by `route(options)`, or by `reject`. `route(options)` creates the path's
  exact `Route`, which inherits the prefix route's identity by default so
  that the reader continues (see [announce takeover](/quest/m1/announce-takeover.md)).
  A prefix route takes cost and auth as an exact one does, so claims can
  be priced. Today `Requests::route()` is always the default and
  `update_route` is crate-private.
- **Visibility, not unannounced broadcasts.** Callers that fill the catalog
  first (`moq-cli` hls, rtsp and publish; moq-studio; moq-mux
  `ts/programs.rs`) or announce after SDP (`moq-rtc/server/whip.rs`) create
  the route hidden and announce it when ready. js/publish's on/off toggle
  becomes withdraw and announce on one route, so viewers continue rather
  than restart.
- **Sessions.** Relay forwarding keeps a crate-private construction with
  forwarded identity, epoch, hops and cost, which the deleted
  `requests_route` and `dynamic_route` did. `moq_net::fuzz`'s hatches
  shrink to what tests still need.
- **JS leaks.** Removing `Requests`/`requestsRoute`/`updateRoute` also
  removes their `@internal`-but-public exposure on `Table`. Stop exporting
  `Epoch.toBytes`/`fromBytes`, `RequestSlot` and `RouteEntry`, and delete
  the orphaned `@internal` doc blocks (`js/net/src/origin.ts:2160-2176`).
- **Bindings.** These all move to the route handle:
  - moq-ffi's `MoqRoute`, `MoqPublication`, `MoqBroadcastRequest` and
    `MoqBroadcastPrepared`.
  - moq-c's `moq_origin_requests`, `moq_broadcast_request_*`,
    `moq_broadcast_prepared_*`, `moq_publication_*` and `moq_mint_epoch`.
  - The py, kt, swift, dart and go wrappers.

  [moq-c's API gaps](/quest/m1/c/api-gaps.md) error-code fix applies to
  whichever functions remain.
- **Callers.** Migrate every caller: about 25 crates use `create_broadcast`
  or `publish`, about 19 set an epoch, plus `@moq/publish`, moq-boy, carla
  and the e2ee generations. The rs/moq-net and js/net `MIGRATE.md` entries
  and the PR's Impact section give the mapping, including moq.pro's
  `.dash` claim and sync-ingest mirror.
- **Docs.** Update `doc/concept/identity.md`'s publishing and lazy-job
  examples, `doc/lib/rs`, `doc/lib/js`, `doc/lib/c`, and each binding's
  README.

Run `just check` and `just test interop --all`.

Public API: breaking across moq-net, @moq/net, moq-ffi, moq-c and the
wrappers. Wire: none.

## Required

- [Identity and epoch](/quest/m1/api-audit/route/identity.md) - the identity a route mints and keeps
- [Serve a held broadcast](/quest/m1/api-audit/route/serve.md) - the served answer this handle carries over

## Related

- [The player takes any read-side origin](/quest/m1/api-audit/resolve/watch-origin.md) - also retypes JS `Table`
- [No constructible internal handles](/quest/m1/api-audit/internal-constructors.md) - the same JS hygiene rule
