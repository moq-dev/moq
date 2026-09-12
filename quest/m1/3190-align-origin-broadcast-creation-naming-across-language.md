# [L] Bindings: create_broadcast, announce, and dynamic mean the same thing in every binding

## Goal

Every native binding exposes the announce surface the Rust model settled on,
with one meaning per name:

- `create_broadcast` (Python, Rust) / `createBroadcast` (Swift, Kotlin, Dart) /
  `CreateBroadcast` (Go) / `moq_origin_create_broadcast` (C) creates an
  unadvertised broadcast.
- `announce(route)` and `unannounce()` on the broadcast producer advertise and
  retract its exact path; announcing again re-prices the route in place.
- `dynamic(pattern, route)` returns the handle that advertises a path pattern
  and serves the requests beneath it. The pattern is a string in the
  `moq_net::path::Pattern` dialect; a prefix is spelled `foo/**`.

No binding announces on the caller's behalf. Every binding's docs state the
same order: create, `dynamic()` for tracks served on demand, populate, then
announce.

## Plan

Today `rs/moq-ffi`'s `create_broadcast` calls `broadcast.announce(..)`
internally (`rs/moq-ffi/src/origin.rs:467-473`), so Python, Swift, Kotlin, Go,
Dart, and C all inherit an auto-announce that Rust does not have.
`MoqOriginProducer::announce(prefix, route)` (`:481`) and
`MoqOriginProducer::dynamic()` (`:429`, no arguments: it advertises the whole
origin through `self.inner.dynamic("", ..)`) both wrap an `origin::Dynamic`,
but only the latter exposes a request queue: the former forwards its requests
into it, and rejects them while no handler is alive. Two handles share one
queue that one of them owns; that is what the rename must not paper over.

The Rust model's `origin::Producer::dynamic(prefix: impl Into<Prefix>, route)`
(`rs/moq-net/src/model/origin.rs:1366`) changes to take a
`moq_net::path::Pattern` (`rs/moq-net/src/path/pattern.rs:172`), so the
announce API breaks once. Until [Advertise](/quest/m1/wildcard-advertise.md)
lands, anything but a prefix-shaped pattern (literal segments then `**`) is
refused; the bindings take the pattern as a string and inherit that refusal.

The convention this documents is what closes #2895. A subscribe by exact path
before the broadcast's tracks exist gets `NotFound` at once
(`rs/moq-net/src/model/broadcast.rs:876-879`) unless a `dynamic()` handler is
alive to park it (`rs/moq-net/src/model/requests.rs:57-60`); announcing only
makes a path discoverable, never reachable (`broadcast.rs:240-242`). So a
publisher creates, attaches `dynamic()` if tracks are served on demand,
populates the catalog, and announces last.

- moq-ffi: `MoqOriginProducer::create_broadcast(path)` stops announcing.
  `MoqBroadcastProducer::set_announce(bool)` (`rs/moq-ffi/src/producer.rs:244`)
  becomes `announce(route: MoqRoute)` plus `unannounce()`.
  `MoqOriginProducer::announce` and `MoqOriginProducer::dynamic()` merge into
  `dynamic(pattern: String, route) -> MoqOriginDynamic` (`:84`), which keeps
  `update(route)`, `requested_broadcast()` (yielding `MoqBroadcastRequest`
  with accept and reject), and `cancel()`; `MoqAnnounce` (`:325`) is deleted.
- libmoq: hard rename `moq_origin_publish` to `moq_origin_create_broadcast`
  with no alias, `moq_publish_set_announce` replaced by
  `moq_publish_announce(route)` and `moq_publish_unannounce`, and
  dynamic/request accessors mirroring the FFI. `rs/libmoq/build.rs`
  regenerates `moq.h`; update `cpp/obs/src/moq-output.cpp:233` and the stub in
  `cpp/obs/test/moq-output-test.cpp:224` that declares the old symbol.
- Wrappers: `py/moq-rs`, `swift`, `kt`, `go/wrapper` (flat on `dev`), and
  `dart` adopt the three verbs and drop any create-and-announce convenience.
  Swift and Python default the route (`announce(route: .init())`,
  `announce(route=Route())`), matching their labeled-argument idiom. For Dart,
  regenerate `dart/moq_ffi` (`just generate` in `dart/`, kixelated/uniffi-dart
  per `dart/README.md`), then adapt `dart/moq/lib/moq.dart` (`:54-56`, whose
  `createBroadcast` is documented as create-and-announce) and
  `dart/moq/test/moq_test.dart` (`:32-35` expects the announcement to follow
  `createBroadcast` on its own).
- Docs: `doc/lib/{py,swift,kt,go,c,dart}/index.md`. The publish examples at
  `py:42`, `go:55`, `kt:40`, `swift:43`, and `dart:34` create a broadcast and
  rely on the auto-announce; each gains the announce call and the create,
  dynamic, populate, announce order. The capability lists (`py:67`, `kt:56`,
  `swift:63`) name a bare `dynamic()`.

Tests: each wrapper covers create, populate, `announce(route)`, visible in
`announced`, then `unannounce()`; `dynamic(pattern, route)` serving a request
under `live/**`; and a non-prefix pattern refused. Run `just test smoke-full`
since the FFI surface changed.

Branch from `dev`: every rename is breaking.

## Closes

- [#3190](https://github.com/moq-dev/moq/issues/3190) - close this issue when the quest finishes

## Related

- [#2152](/quest/m2/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the rest of the C ABI catch-up
- [JS announce](/quest/m1/js-announce.md) - the same alignment for js/net
- [Advertise](/quest/m1/wildcard-advertise.md) - lifts the prefix-only refusal so `dynamic()` accepts any pattern
