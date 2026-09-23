# [M] An origin announces what it can serve

## Goal

The announce cursor and the broadcast resolver agree on what exists. Today
`create_broadcast` publishes a path that `request_broadcast` will happily
serve but that no consumer could have discovered: nothing crossed the cursor.
Resolving a broadcast that was never advertised is strange on its face, and
it forces every waiter to carry two notions of reachable.

## Plan

Two shapes are on the table, and this quest picks one:

- `create_broadcast` records an announcement scoped to this origin and never
  forwarded to a peer. The cursor becomes the complete local inventory and
  the resolver follows it.
- `request_broadcast` stops resolving a local broadcast that was not
  announced, so publishing and advertising stay two deliberate steps and the
  cursor stays the only discovery path.

The first is the leaning: serving a path while hiding it from the local
inventory is the odd combination, and a relay's own ingest is exactly what a
local consumer wants to find.

Things to settle along the way:

- A local announcement must not reach a peer. Prove it with a test rather
  than a comment. The outgoing announce path already filters by scope, so the
  question is whether that filter is the right seam or whether the route
  needs to carry the fact.
- Whether a producer's own consumer should see its own announcements at all.
  It arguably should not, and the decision belongs here because it decides
  what "local" scopes to.
- `origin::Consumer::routed` walks the announce cursor while the path `Watch`
  watches the route table. Once the cursor reports every servable path the
  two report the same events. #3901 already collapsed `routed_broadcast` onto
  the watch; this is what makes that collapse principled rather than a
  widening of what resolves.
- `announced_broadcast` and its libmoq, Python, Swift, Kotlin, Go, and Dart
  faces describe the wait in prose. That wording moved in #3901 and moves
  again here; keep it honest, and remember UniFFI hashes doc comments into
  the API checksum, so the generated bindings need regenerating with it.

Public API: additive on `moq-net` and `@moq/net` if a local announcement is a
new route source, behavioral for anything reading the cursor. Wire: none, and
a test should say so rather than the PR description.

## Related

- [Ingest source](/quest/next/net-ingest-source.md) - the neighbouring question of where a route entered
- [Front parking](/quest/next/origin-front-parks.md) - the other half of retiring `routed_broadcast`'s retry loop
