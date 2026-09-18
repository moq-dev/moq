# [M] An announce event says what it covers without a fallible unwrap

## Goal

A consumer of `announce::Consumer` reads the covered path and what changed
in one match. Today `AnnounceUpdate { pattern, route, active }` carries a
`Pattern` even though wildcard claims cannot be resolved into subscriptions
yet, so every consumer opens with `pattern.as_prefix().expect("prefix
announcement")`: 80 sites in this repository and nine in moq.pro. The same
consumers keep a `HashSet` beside the cursor to tell a new broadcast from a
route reprice, because `active: true` twice means metadata.

## Plan

Decide the shape, then apply it in Rust, JS, moq-ffi, libmoq, and every
binding at once, since the event is published API in every language:

- Recommended: `announce::Update { claim: Claim, captures, route: Route, kind: Kind }`
  with `Claim::{Prefix(PathOwned), Pattern(Pattern)}` so the common case is
  infallible and a wildcard advertisement still rides the same cursor, and
  `Kind::{Announced, Updated, Retracted}` replacing the boolean. `captures`
  is what PR #3746 adds and stays as it lands there. The retracted event
  keeps its last `route` in JS the way Rust already does.
- Alternative: keep `pattern` and add `path()`/`kind`; cheaper, but leaves
  the unwrap in place for the prefix case.
- Delete `Announce.Update.anonymous` in JS (`js/net/src/announced.ts`); it is
  computed from `route` and `Origin.isAnonymous(route)` is exported.
- Give `announce::Consumer` a `futures::Stream` impl in Rust and
  `[Symbol.asyncIterator]` in JS; moq.pro wrote an `Announced` class and the
  demo, room, and watch packages each carry the same `Promise.race` drain loop.
- [Bindings announce match](/quest/m1/api-origin-scopes.md) mirrors the
  shape settled here, so it takes this as a blocker once #3746 is in.

Public API: breaking on moq-net, @moq/net, moq-ffi, libmoq, and the bindings,
so on dev. Wire: none. Consumers: moq-relay, moq-stats, moq-cli, the JS
packages, `demo/web`, and moq.pro's recorder, stats, and ingest loops.

## Related

- [Bindings announce match](/quest/m1/api-origin-scopes.md) - the binding half, which takes the shape settled here
- [Wildcard](/quest/m2/wildcard/README.md) - the resolution that makes a pattern claim usable
- [Ingest source](/quest/m2/net-ingest-source.md) - a further field the same event should carry
