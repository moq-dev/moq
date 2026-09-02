# [M] Bindings: create_broadcast, set_announce, and announce mean the same thing everywhere

## Goal

Every native binding exposes the announce surface the Rust model settled on,
with one meaning per name:

- `create_broadcast` (Python, Rust) / `createBroadcast` (Swift, Kotlin) /
  `CreateBroadcast` (Go) / `moq_origin_create_broadcast` (C) creates an
  unadvertised broadcast.
- `set_announce(bool)` on the broadcast producer flips its exact-path advert.
- `announce(prefix, route)` returns the handle that advertises and serves
  requests beneath the prefix.

No binding announces on the caller's behalf any more.

## Plan

Today `rs/moq-ffi`'s `create_broadcast` calls `announce(path, ..)` internally
and stores the guard, so Python, Swift, Kotlin, Go, and C all inherit an
auto-announce that Rust does not have, while `MoqOriginDynamic` is a separate
type from `MoqAnnounce`. Two surfaces share a name and differ; that is what
the rename must not paper over.

- moq-ffi: `MoqOriginProducer::create_broadcast(path)` stops announcing.
  `MoqBroadcastProducer::set_announce(bool)` stays as the flip, now defaulting
  to `false`. `MoqAnnounce` gains the request queue (`requested()` yielding
  `MoqBroadcastRequest` with accept and reject) and `MoqOriginDynamic` is
  deleted.
- libmoq: hard rename `moq_origin_publish` to `moq_origin_create_broadcast`
  with no alias, `moq_publish_set_announce` kept, and announce/request
  accessors mirroring the FFI. Regenerate `moq.h`; update `cpp/obs/src` and
  the `cpp/obs/test` stub that declares the old symbol.
- Wrappers: `py/moq-rs`, `swift`, `kt`, and `go/wrapper` (flat on `dev`)
  adopt the three verbs and drop any create-and-announce convenience.
  `dart/` exists only on `main`, so it has its own quest gated on the merge.
- Docs: `doc/lib/{py,swift,kt,go,c}`, including the `doc/lib/py/moq-rs.md`
  sentence that says `create_broadcast` creates an announced broadcast.

Tests: each wrapper covers create, populate, `set_announce(true)`, visible in
`announced`; and `announce(prefix)` serving a request. Run `just test
smoke-full` since the FFI surface changed.

Branch from `dev`: every rename is breaking.

## Required

- [Announce handle](/quest/m1/announce-handle.md) - the Rust surface these bindings mirror

## Closes

- [#3190](https://github.com/moq-dev/moq/issues/3190) - close this issue when the quest finishes

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the rest of the C ABI catch-up
- [JS announce](/quest/m1/js-announce.md) - the same alignment for js/net
- [Dart announce](/quest/m1/dart-announce.md) - the Dart wrapper, once `dev` merges
