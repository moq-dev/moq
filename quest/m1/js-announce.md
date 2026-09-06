# [M] js/net: createBroadcast, a broadcast-owned announcement, and the dynamic handle

## Goal

`Origin.Producer` in js/net has the same three operations as the Rust model:
`createBroadcast(path)` creates an unadvertised broadcast, the broadcast
producer's `announce(route)` / `unannounce()` advertise its exact path once it
is populated, and `dynamic(prefix, route)` returns a handle that advertises the
prefix and yields the requests beneath it for the app to accept or reject.

## Plan

On `dev`, `Origin.Producer.publish(path)` creates and announces in one step
through the local table, and `announce(prefix, provider)` takes a
`RouteProvider` that serves requests through an interface the caller
implements. Both fuse two decisions and the second is callback-shaped.

- Rename `publish(path)` to `createBroadcast(path)`, unadvertised. The
  broadcast producer gains `announce(route)` and `unannounce()`; the origin
  keeps the association in its table and retracts when the producer closes.
  Announcing again re-prices in place, so a route knob in the signals idiom
  (see `js/CLAUDE.md`) is the natural backing.
- js/net has no `Route` type today: `Hop[]` lives in `hop.ts` and `Cost` in
  `lite/announce.ts`, and the origin never sees either. Add one (hops plus
  cost) so the origin API and the wire agree, and stamp it on
  `announce.Event` so consumers can read it back.
- `dynamic(prefix, route)` returns a `Dynamic` handle: `update(route)`,
  `close()` to retract and reject, and `requested()` as an async iterator of
  requests with `accept(broadcast)` and `reject(error)`. `RouteProvider` is
  removed; `forward.ts` and the session code drive the handle instead.
- Consumers: `js/publish` (whose `announce` attribute becomes the flip rather
  than a gate on running at all), `js/watch`, `js/boy`, `js/clock`, `demo/web`,
  and the `doc/lib/js/@moq` pages that show `publish(path)` or
  `announce(prefix, provider)`.

Tests: `origin.test.ts` and `integration.test.ts` cover create then announce,
a handle serving a request under its prefix, and close rejecting queued
requests; `reload.test.ts` keeps the announce state across a reload.

Branch from `dev`, where the origin table lives; the rename is breaking.

## Related

- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - the native bindings half
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - other js/net gaps vs rs/moq-net
