# [XL] One Route handle

## Goal

An application publishes by announcing a route and holding the `Route`
handle it gets back. `Publication`, `Prepared`, `Announcer`, `Requests` and
`Producer::publish` are gone, in Rust, JS, moq-ffi, moq-c and the wrappers:

```rust
// An exact route serves one broadcast; dropping the route withdraws it.
let broadcast = broadcast::Producer::new();
broadcast.create_track("catalog", info)?;
let route = origin.announce("live/alice", broadcast, Options::default())?;
route.set_cost(5); // re-prices in place, same instance

// A prefix route serves requests.
let claim = origin.announce_prefix("transcode", Options::default())?;
while let Some(request) = claim.requested().await {
    // Either serve a broadcast already held, under the claim's instance...
    // request.serve(held);
    // ...or announce the path as its own route and instance.
    let job = request.announce(broadcast::Producer::new(), Options::default())?;
    spawn(run(job));
}
```

Apps pass only options: `cost`, `auth`, and an `Instance` override for
re-publishing a run they did not start (a mirror, archive resume, FFI
pass-through). Without the override, `announce` mints a new instance under
the origin's identity on every call, so re-announcing a broadcast is a
restart and never a splice. Hops, via, source and the anonymous mark become
read-only facts on a received announcement; `Hop`, `Hops` and `HopLimit`
leave the public API.

Non-goals: the instance model ([Identity and epoch](/quest/m1/api-audit/route/identity.md)),
announce events ([Listing and follow](/quest/m1/api-audit/route/events.md)),
and the route layer's wire metric ([Routes and announces](/quest/m1/cluster-routing/routes.md)).

## Plan

Decided 2026-10-30 (maintainer); see the [epic](/quest/m1/api-audit/route/README.md).
The names above are a sketch. Settle the final ones in the PR, keeping Rust,
JS and the bindings identical.

- **One type.** An exact route derefs to the `broadcast::Producer` it
  serves. A prefix route yields requests. Each request is answered by
  `serve(consumer)` (from [serve](/quest/m1/api-audit/route/serve.md)),
  by `announce(broadcast, options)`, which returns that path's exact
  `Route`, or by `reject`. A prefix route takes cost and auth just as an
  exact one does, so claims can be priced. Today `Requests::route()` is
  always the default and `update_route` is crate-private.
- **No unannounced state.** Today's `create_broadcast`-then-`announce`
  callers become a plain `broadcast::Producer` that is announced once it
  is ready. They are: catalog first (`moq-cli` hls, rtsp and publish;
  moq-studio; moq-mux `ts/programs.rs`), announce after SDP
  (`moq-rtc/server/whip.rs`), and js/publish's on/off toggle.
- **Sessions.** Relay forwarding keeps its crate-private construction with
  forwarded instance, hops and cost, which the deleted `requests_route`
  and `dynamic_route` did. `moq_net::fuzz`'s hatches shrink to what tests
  still need.
- **JS leaks.** Removing `Requests`/`requestsRoute`/`updateRoute` also
  removes their `@internal`-but-public exposure on `Table`. Stop exporting
  `Epoch.toBytes`/`fromBytes`, `RequestSlot` and `RouteEntry`, and delete
  the orphaned `@internal` doc blocks (`js/net/src/origin.ts:2160-2176`).
- **Bindings.** moq-ffi's `MoqRoute`/`MoqPublication`/
  `MoqBroadcastRequest`/`MoqBroadcastPrepared`, moq-c's `moq_origin_requests`,
  `moq_broadcast_request_*`, `moq_broadcast_prepared_*`, `moq_publication_*`
  and `moq_mint_epoch`, and the py/kt/swift/dart/go wrappers all move to
  the route handle. [moq-c's API gaps](/quest/m1/c/api-gaps.md) error-code
  fix applies to whichever functions remain.
- **Callers.** Migrate every caller: about 25 crates use `create_broadcast`
  or `publish`, about 19 set an epoch, plus `@moq/publish`, moq-boy, carla
  and the e2ee generations. The rs/moq-net and js/net `MIGRATE.md` entries
  and the PR's Impact section give the mapping, including moq.pro's
  `.dash` claim and sync-ingest mirror.
- **Docs.** Update `doc/concept/identity.md`'s publishing and lazy-job
  examples, `doc/lib/rs`, `doc/lib/js` and `doc/lib/c`, and each binding's
  README.

Run `just check` and `just test interop --all`.

Public API: breaking across moq-net, @moq/net, moq-ffi, moq-c and the
wrappers. Wire: none.

## Required

- [Identity and epoch](/quest/m1/api-audit/route/identity.md) - the instance the handle's options and minting name
- [Serve a held broadcast](/quest/m1/api-audit/route/serve.md) - the served answer this handle carries over

## Related

- [The player takes any read-side origin](/quest/m1/api-audit/resolve/watch-origin.md) - also retypes JS `Table`
- [No constructible internal handles](/quest/m1/api-audit/internal-constructors.md) - the same JS hygiene rule
