# [M] js/net: createBroadcast, an announce flag, and an announce handle

## Goal

`Origin.Producer` in js/net has the same three operations as the Rust model:
`createBroadcast(path)` creates an unadvertised broadcast, the producer's
announce flag advertises its exact path once it is populated, and
`announce(prefix, route)` returns a handle that advertises the prefix and
yields the requests beneath it for the app to accept or reject.

## Plan

On `dev`, `Origin.Producer.publish(path)` creates and announces in one step
through the local table, and `announce(prefix, provider)` takes a
`RouteProvider` that serves requests through an interface the caller
implements. Both fuse two decisions and the second is callback-shaped.

- Rename `publish(path)` to `createBroadcast(path)`, unadvertised. The
  broadcast producer carries a settable announce flag in the signals idiom
  (see `js/CLAUDE.md`), default off, retracted when the producer closes.
- `announce(prefix, route)` returns an `Announce` handle: `update(route)`,
  `close()` to retract and reject, and `requested()` as an async iterator of
  requests with `accept(broadcast)` and `reject(error)`. `RouteProvider` is
  removed; `forward.ts` and the session code drive the handle instead.
- Consumers: `js/publish`, `js/watch`, `js/boy`, `demo/web`, and the
  `doc/lib/js/@moq` pages that show `publish(path)`.

Tests: `origin.test.ts` and `integration.test.ts` cover create then flag,
a handle serving a request under its prefix, and close rejecting queued
requests; `reload.test.ts` keeps the announce state across a reload.

Branch from `dev`, where the origin table lives; the rename is breaking.

## Required

- [Announce handle](/quest/m1/announce-handle.md) - the shape is proven in Rust first

## Related

- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - the native bindings half
- [#2985](/quest/m1/2985-js-net-path-keyed-publisher-state-goes-stale-when-a.md) - publisher state on the same origin table
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - other js/net gaps vs rs/moq-net
