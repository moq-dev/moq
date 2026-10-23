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

Decided 2026-10-23 (maintainer, on
[moq-dev/moq#6464](https://github.com/moq-dev/moq/pull/6464)): ✅ the
announcement handle is `origin::Publication`, an origin-owned record holding the
fill broadcast and its announcement capability. `create_broadcast`, `publish`,
and an admitted `Prepared::publish` return it; `Prepared::broadcast` and
standalone producers stay fill-only. It borrows its fill through `broadcast()`,
forwards immutable fill operations (no `DerefMut`, so a caller cannot swap in an
unrelated producer), and `finish()` hands off to the existing `Finish`. Clones,
fill clones, and a `Finish` keep the existing broadcast lifetime; the last
`Publication` dropping does not retract on its own. Reason: it keeps today's
linger and fill retention without a second route mechanism, while a literal
producer/announcer pair would make every media holder and finish path carry both.
Do not export an optional capability on a fill producer to avoid the migration.

The native side (moq-net, its Rust holders, examples, benchmarks, and private
C/UniFFI adapters) is migrated. Remaining: mirror `Publication` in `@moq/net`,
moq-ffi, the C ABI, and every wrapper (py, swift, kt, dart, go, cpp, obs),
regenerate outputs, and update their consumers, docs, and `MIGRATE.md` entries.
Exercise clone/drop, explicit unannounce, finished tails, Prepared admission, and
stale owners against a replacement publication in each. A finished-but-open
Prepared answer remains publishable and serveable through its retained `Finish`;
a missing binding fill registry/state after finish does not mean the native
broadcast is closed. Keep deferred validity and request role redesigns separate.

Public API: breaking in moq-net (and `@moq/net`/moq-ffi if they mirror the
trap). Wire: none.

## Related

- [Claim request type](/quest/m1/public-model/claim-request-type.md) - splits `accept` off application requests in the same file; coordinate the order
- [Public routes and answering](/quest/m1/public-model/answer.md) - later makes the forwarding claim public
