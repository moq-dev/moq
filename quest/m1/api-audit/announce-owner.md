# [M] The origin announces

## Goal

A `broadcast::Producer` fills tracks; it cannot announce. Announcing is a
handle the origin hands out, so a `Prepared` answer and a standalone
`Info::produce()` broadcast no longer expose `announce`, `unannounce`, or
`route` calls that can only fail.

## Plan

Decided 2026-10-22 (API audit). Today `Prepared::broadcast()`
(`rs/moq-net/src/model/origin.rs`) returns a cloneable producer whose
`announce` fails with `Error::Closed` (documented in `MIGRATE.md`), and a
standalone broadcast's `announce`, `unannounce`, and `route` can never work
(inherited). The prior audit called the `Prepared` case a panic; it is an
error.

- `origin::Producer::create_broadcast` (and `publish`) return the broadcast
  plus an origin-owned announcement handle that carries `route`/`unannounce`.
- `Prepared` exposes only the fill surface; `publish` stays the one way to
  answer.
- Mirror the shape in `@moq/net` if its producer has the same trap, and in
  moq-ffi where the binding exposes announce on the broadcast.

The draft proposes an `origin::Publication` owner that combines the fill
broadcast and its real announcement capability. `create_broadcast`, `publish`,
and an admitted `Prepared::publish` return it; `Prepared::broadcast` and
standalone producers remain fill-only. The proposed record forwards fill
operations and exposes the borrowed producer through `broadcast()`.

The return/lifetime shape still needs review. The current proposal preserves
last-fill and `Finish` retention rather than making the last announcement
capability clone retract independently. A literal producer/announcement pair
would make that transfer explicit but requires media holders and finish/linger
paths to retain both. Settle this choice before mirroring the record in JS,
moq-ffi, C, and every wrapper; do not export an optional capability on a fill
producer to avoid the migration.

The native proposal migrates current Rust holders, examples, benchmarks, and
private C/UniFFI adapters. Remaining work is accepting the return/lifetime shape,
mirroring the public role in JS and all bindings, regenerating outputs from
source, and updating those consumers and package migrations. Exercise clone/drop,
explicit unannounce, finished tails, Prepared admission, and stale owners
against a replacement publication. A finished-but-open Prepared answer remains
publishable and serveable through its retained `Finish`; a missing binding fill
registry/state after finish does not mean the native broadcast is closed. Keep
deferred validity and request role redesigns separate.

Public API: breaking in moq-net (and `@moq/net`/moq-ffi if they mirror the
trap). Wire: none.

## Related

- [Claim request type](/quest/m1/public-model/claim-request-type.md) - splits `accept` off application requests in the same file; coordinate the order
- [Public routes and answering](/quest/m1/public-model/answer.md) - later makes the forwarding claim public
