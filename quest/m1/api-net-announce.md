# [M] Publishers announce prefixes; consumers read trimmed paths

## Goal

A broadcast or dynamic route is announced as a path prefix, on every wire
version, and a consumer scoped by any pattern reads the covered path
already trimmed to its scope. Today `AnnounceUpdate { pattern, route,
active }` carries a `Pattern` even for the prefix case, so every consumer
opens with `pattern.as_prefix().expect("prefix announcement")` (80 sites
here, nine in moq.pro) and keeps a `HashSet` beside the cursor to tell a
new broadcast from a route reprice.

## Plan

Decided 2026-09-18 by the maintainer; a branch may already claim this, so
check open PRs before starting.

- Publishing is prefix-only. `origin::Producer::dynamic(prefix, route)` and
  `broadcast::Producer::announce` take a path, not a pattern, so the same
  claim rides moq-lite and moq-transport alike. `ANNOUNCE_PATTERN` leaves
  the lite-06-wip draft; PR #3746's non-prefix presented patterns go with
  it.
- Consuming stays a pattern. `announced(scope)` accepts any `Pattern`; the
  network or the library drops claims that do not overlap the scope, and
  `captures` (what each scope wildcard stood for, per #3746) stays.
- The event is `announce::Update { path: PathOwned, captures, route: Route,
  kind: Kind }` with `Kind::{Announced, Updated, Retracted}` replacing the
  boolean; `path` is the covered prefix relative to the consumer's root,
  trimmed by the library so no caller does it. The retracted event keeps
  its last `route` in JS the way Rust already does.
- Delete `Announce.Update.anonymous` in JS (`js/net/src/announced.ts`); it
  is computed from `route` and `Origin.isAnonymous(route)` is exported.
- `announce::Consumer` gets a `futures::Stream` impl in Rust and
  `[Symbol.asyncIterator]` in JS; moq.pro wrote an `Announced` class and
  the demo, room, and watch packages each carry the same `Promise.race`
  drain loop.
- [Bindings announce match](/quest/m1/api-origin-scopes.md) mirrors this
  shape and takes it as a blocker; the advertise half of
  [Wildcard](/quest/m2/wildcard/README.md) is re-scoped to prefix claims
  resolved against pattern interest.

Public API: breaking on moq-net, @moq/net, moq-ffi, libmoq, and the
bindings, so on dev. Wire: `ANNOUNCE_PATTERN` is removed from
`drafts/draft-lcurley-moq-lite.md` (lite-06-wip); run `just drafts check`.
Consumers: moq-relay, moq-stats, moq-cli, the JS packages, `demo/web`,
and moq.pro's recorder, stats, and ingest loops.

## Related

- [Bindings announce match](/quest/m1/api-origin-scopes.md) - the binding half, which takes the shape settled here
- [Wildcard](/quest/m2/wildcard/README.md) - the resolution side, now against prefix claims
- [Pattern interest](/quest/m2/path-patterns/interest.md) - the consumer pattern on the lite-06 wire
- [Ingest source](/quest/m2/net-ingest-source.md) - a further field the same event should carry
