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

Public API: breaking in moq-net (and `@moq/net`/moq-ffi if they mirror the
trap). Wire: none.

## Related

- [Claim request type](/quest/m1/api-audit/claim-request-type.md) - splits `accept` off application requests in the same file; coordinate the order
