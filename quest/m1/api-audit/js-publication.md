# [M] JS origin publications

## Goal

A `@moq/net` `broadcast.Producer` fills tracks; it cannot announce. Creating a
broadcast on an origin and publishing a `Prepared` answer return a
`Publication` that carries `announce`, `unannounce`, and `route`, so a
standalone broadcast and `prepared.broadcast` no longer expose announcement
calls that can only throw. Mirrors Rust's `origin::Publication`.

## Plan

Decided 2026-10-23 (maintainer, on
[moq-dev/moq#6464](https://github.com/moq-dev/moq/pull/6464)): ✅ land the
announcement split in three PRs: native Rust in that PR, this `@moq/net`
mirror, and the [binding publications](/quest/m1/api-audit/binding-publication.md).
This one does not depend on the native PR and can start now.

The shape is Rust's, approved on the same PR: an origin-owned record holding
the fill broadcast and its announcement capability.

- `Producer.createBroadcast` (`js/net/src/origin.ts`) and `Prepared.publish`
  return `Publication`. It exposes the fill producer as `broadcast`, plus
  `announce(route)`, `unannounce()`, and `route`. No way to swap in another
  producer, and no `finish`: a broadcast has no finished state.
- `broadcast.Producer` loses its public `announce`, `unannounce`, and `route`
  (`js/net/src/broadcast.ts`); the announcer stays a module hook.
- Lifetime matches Rust: publication clones (or references) and fill handles
  keep the existing broadcast lifetime; dropping a `Publication` does not
  retract on its own. `unannounce` retracts sooner.
- Migrate every caller: `js/net` tests, benches, and examples, `js/publish`,
  `js/moq-boy`, the `js/e2ee` docs, `js/watch` and `js/hang` tests, and
  `demo/carla`. Add the `js/net/MIGRATE.md` entry and update
  `doc/lib/js/net.md`.

Test: clone/drop, explicit unannounce, Prepared admission,
and a stale publication against a replacement under a new epoch.

Public API: breaking in `@moq/net`. Wire: none.
