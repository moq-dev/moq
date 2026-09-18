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
- Consuming is a prefix too, with an optional wildcard filter on the
  consume side. `announced(prefix)` opens exactly one ANNOUNCED or
  SUBSCRIBE_NAMESPACE for that prefix on moq-lite and moq-transport alike;
  a caller wanting `room/*/chat` passes `room` and filters the cursor with
  the pattern (`announced(prefix).matching(pattern)`, or a filter argument
  with the same effect). The library never derives heads from a pattern
  union or multiplexes members; two heads are two calls. The pattern lives
  in two places only: the token, which the relay enforces by scoping its
  origin handle, and that local filter. `captures` (what each filter
  wildcard stood for) is derived from the announced prefix against the
  filter, so it stays and is `None` when no filter is set or the prefix
  does not pin it.
- Announcements are hints; requests are the authority. A prefix route
  `room` that overlaps a grant of `room/*/chat` is forwarded as
  `ANNOUNCE_START room`, and a request for `room/bob/video` is refused by
  `matches` at the relay, the same contract a dynamic route has today. No
  set-valued intersection, clamp, or tie-break. The lite draft says so in
  one sentence, so nobody reintroduces a narrowing message to fix the
  over-claim.
- The cost is accepted: a scope with an empty literal head (`**/chat`)
  subscribes to every announcement and filters locally. Announcements are
  per broadcast and tens of bytes; a per-connection announce budget or a
  head on the pattern is the answer if a relay ever measures it, not a wire
  hint.
- The event is `announce::Update { path: PathOwned, captures:
  Option<Captures>, route: Route, kind: Kind, source: Source }` with
  `Kind::{Announced, Updated, Retracted}` replacing the boolean; `path` is
  the covered prefix relative to the consumer's root, trimmed by the library
  so no caller does it. `captures` is `Some` only when the announced prefix
  pins every wildcard of the filter: a broadcast at `room/alice/chat` under
  `room/*/chat` captures `alice`, a dynamic claim of `room` under the same
  filter overlaps but pins nothing and carries `None`. `source` is
  `Local` or `Peer(Hop)` from the origin's bookkeeping
  ([ingest source](/quest/m2/net-ingest-source.md) fills it in; the field
  is settled here so it is never added after the release). The retracted
  event keeps its last `route` in JS the way Rust already does.
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
bindings, so on dev. Wire: `ANNOUNCE_PATTERN` and the `Patterns` section
leave `drafts/draft-lcurley-moq-lite.md` (lite-06-wip), and
`NAMESPACE_PATTERN` and its setup option leave
`drafts/draft-lcurley-moq-pattern.md`, which keeps only the matching and
authorization rules the token and the library share; run `just drafts
check`. Two PRs: this one carries the prefix table and the full event
shape (`path`, `Kind`, `Stream`/`asyncIterator`, no `anonymous`); PR #3746
rebases onto it as [Bindings announce match](/quest/m1/api-origin-scopes.md)
and keeps only its surviving half.
Consumers: moq-relay, moq-stats, moq-cli, the JS packages, `demo/web`,
and moq.pro's recorder, stats, and ingest loops.

## Related

- [Bindings announce match](/quest/m1/api-origin-scopes.md) - the binding half, which takes the shape settled here
- [Wildcard](/quest/m2/wildcard/README.md) - the resolution side, now against prefix claims
- [Pattern grants](/quest/m2/path-patterns/interest.md) - AUTH carries pattern grants; ANNOUNCE_REQUEST stays a prefix by this decision
- [Ingest source](/quest/m2/net-ingest-source.md) - a further field the same event should carry
