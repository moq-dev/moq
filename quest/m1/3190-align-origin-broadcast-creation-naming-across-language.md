# [M] Bindings: create_broadcast, announce, and dynamic mean the same thing everywhere

## Goal

Every native binding exposes the announce surface the Rust model settled on,
with one meaning per name:

- `create_broadcast` (Python, Rust) / `createBroadcast` (Swift, Kotlin) /
  `CreateBroadcast` (Go) / `moq_origin_create_broadcast` (C) creates an
  unadvertised broadcast.
- `announce(route)` and `unannounce()` on the broadcast producer advertise and
  retract its exact path; announcing again re-prices the route in place.
- `dynamic(prefix, route)` returns the handle that advertises the prefix and
  serves the requests beneath it.

No binding announces on the caller's behalf any more.

## Plan

Today `rs/moq-ffi`'s `create_broadcast` calls `broadcast.announce(..)`
internally, so Python, Swift, Kotlin, Go, and C all inherit an auto-announce
that Rust does not have. `MoqOriginProducer::announce(prefix, route)` and
`MoqOriginProducer::dynamic()` both wrap the same `origin::Dynamic`, but only
the latter exposes its request queue and the former parks whatever is requested
beneath it. Two surfaces share a type and differ; that is what the rename must
not paper over.

- moq-ffi: `MoqOriginProducer::create_broadcast(path)` stops announcing.
  `MoqBroadcastProducer::set_announce(bool)` becomes `announce(route: MoqRoute)`
  plus `unannounce()`. `MoqOriginProducer::announce` and `MoqOriginDynamic` merge
  into `dynamic(prefix, route) -> MoqOriginDynamic`, which keeps `update(route)`,
  `requested_broadcast()` (yielding `MoqBroadcastRequest` with accept and
  reject), and `cancel()`; `MoqAnnounce` is deleted.
- libmoq: hard rename `moq_origin_publish` to `moq_origin_create_broadcast`
  with no alias, `moq_publish_set_announce` replaced by
  `moq_publish_announce(route)` and `moq_publish_unannounce`, and
  dynamic/request accessors mirroring the FFI. Regenerate `moq.h`; update
  `cpp/obs/src` and the `cpp/obs/test` stub that declares the old symbol.
- Wrappers: `py/moq-rs`, `swift`, `kt`, and `go/wrapper` (flat on `dev`)
  adopt the three verbs and drop any create-and-announce convenience. Swift
  and Python default the route (`announce(route: .init())`, `announce(route=
  Route())`), matching their labeled-argument idiom. `dart/` exists only on
  `main`, so it has its own quest gated on the merge.
- Docs: `doc/lib/{py,swift,kt,go,c}`, including the `doc/lib/py/moq-rs.md`
  sentences that say `create_broadcast` creates an announced broadcast and pair
  `announce("live/")` with a prefix-less `dynamic()`.

Tests: each wrapper covers create, populate, `announce(route)`, visible in
`announced`, then `unannounce()`; and `dynamic(prefix, route)` serving a
request. Run `just test smoke-full` since the FFI surface changed.

Branch from `dev`: every rename is breaking.

## Closes

- [#3190](https://github.com/moq-dev/moq/issues/3190) - close this issue when the quest finishes

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the rest of the C ABI catch-up
- [JS announce](/quest/m1/js-announce.md) - the same alignment for js/net
- [Dart announce](/quest/m1/dart-announce.md) - the Dart wrapper, once `dev` merges
