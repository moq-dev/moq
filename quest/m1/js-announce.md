# [M] js/net: createBroadcast, a broadcast-owned announcement, and the dynamic handle

## Goal

`Origin.Producer` in js/net has the same three operations as the Rust model:
`createBroadcast(path)` creates an unadvertised broadcast, the broadcast
producer's `announce(route)` / `unannounce()` advertise its exact path once it
is populated, and `dynamic(pattern, route)` returns a handle that advertises a
path pattern (a prefix is spelled `foo/**`) and yields the requests beneath it
for the app to accept or reject.

## Plan

On `dev`, `Origin.Producer.publish(path)` (`js/net/src/origin.ts:237`) creates
and announces in one step through the local table, and
`announce(prefix, provider)` (`:279`) takes a `RouteProvider` (`:46`) that
serves requests through an interface the caller implements. Both fuse two
decisions and the second is callback-shaped.

- Rename `publish(path)` to `createBroadcast(path)`, unadvertised. The
  broadcast producer gains `announce(route)` and `unannounce()`; the origin
  keeps the association in its table and retracts when the producer closes.
  Announcing again re-prices in place, so a route knob in the signals idiom
  (see `js/CLAUDE.md`) is the natural backing.
- js/net has no `Route` type today: `Hop` lives in `hop.ts:28` and `Cost` in
  `lite/announce.ts:30`, and the origin never sees either. Add one (hops plus
  cost) so the origin API and the wire agree, and stamp it on
  `announce.Event` so consumers can read it back.
- `dynamic(pattern, route)` returns a `Dynamic` handle: `update(route)`,
  `close()` to retract and reject, and `requested()` as an async iterator of
  requests with `accept(broadcast)` and `reject(error)`. The pattern is parsed
  by `Path.Pattern` (`js/net/src/path.ts:526`), the same dialect the Rust
  model takes, so the API breaks once; until
  [Advertise](/quest/m1/wildcard/advertise.md) lands, anything but a
  prefix-shaped pattern is refused. `RouteProvider` is removed;
  `connection/forward.ts` and the session code drive the handle instead.
- `StreamCode` (`js/net/src/error.ts:71-95`) gains `NoCapacity: 0x30`, the
  refusal a dynamic handle sends when it could serve a request but has no room
  (`drafts/draft-lcurley-moq-lite.md:311`); the Rust side already has it.
- Consumers: `js/publish` (`js/publish/src/broadcast.ts:212`, whose `announce`
  attribute becomes the flip rather than a gate on running at all),
  `js/moq-boy` (`js/moq-boy/src/game.ts:303`), `js/clock`
  (`js/clock/src/main.ts:81`), and the docs: `doc/lib/js/net.md` shows
  `origin.publish` (`:24`) and `announce(prefix, provider)` (`:41`), and
  `doc/lib/js/publish.md:34` describes the `announce` attribute. Both pages
  state the order the model wants: create, `dynamic()` for tracks served on
  demand, populate, then announce, because an exact-path subscribe before the
  tracks exist is refused and announcing only makes a path discoverable.

Tests: `origin.test.ts` and `integration.test.ts` cover create then announce,
a handle serving a request under `live/**`, a non-prefix pattern refused, and
close rejecting queued requests with `NoCapacity`;
`js/net/src/connection/reload.test.ts` keeps the announce state across a
reload.

Branch from `dev`, where the origin table lives; the rename is breaking.

## Related

- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - the native bindings half, and the Rust `dynamic(pattern, route)` signature
- [#2774](/quest/m1/2774-collapse-reload-and-shared-into-one-connection-class.md) - rewrites `connection/reload.ts` and `pool.ts`, which this touches; land one before the other
- [Advertise](/quest/m1/wildcard/advertise.md) - lifts the prefix-only refusal so `dynamic()` accepts any pattern
- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - other js/net gaps vs rs/moq-net
