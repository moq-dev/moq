# [M] Scopes are patterns and every binding reports the match

## Goal

An origin handle is scoped by any pattern union (`room/*/chat`, `**/a`,
exact `foo`), a route is visible to a cursor when the announced prefix
overlaps a scope member, a request is checked with `matches`, and the
announce event reports `captures`, what each scope wildcard stood for. Every
binding that exposes announcements takes the same pattern scope and reports
the same match. `moq-ffi`'s `MoqOriginConsumer::announced(prefix)` scopes by
a literal root today, so its only wildcard is the implicit trailing `**`.

## Plan

This is PR #3746 (branch `quest/m1/api-origin-scopes`) rebased onto
[announce event](/quest/m1/api-net-announce.md) and re-scoped to what
survives once route entries are prefixes and the wire carries no pattern:

- `scope(root, &Patterns)` accepts any union and intersects; `with_root`
  refuses only a root nothing lies under; `allowed` is the set-valued
  rebase. Visibility is `overlaps(prefix/**, member)`; `create_broadcast`,
  `resolve`, and `request_broadcast` check `matches`. `dynamic(prefix)`
  under a wildcard grant is accepted when the prefix overlaps the grant and
  its requests are filtered, which is what makes a wildcard grant usable
  with prefix interest on the wire.
- Deleted from #3746: the `Pattern` route table, the runtime
  `Pattern::intersect` clamp and tie-break, `ANNOUNCE_PATTERN` on lite-06,
  and any lite-versus-transport special path. `Pattern::intersect` and
  `captures` stay as library helpers.
- The relay's `AuthToken::new` keeps any grant as written and
  `AuthError::UnsupportedPattern` goes; `moq_net::stats::Config::exclude`
  is a `Patterns`. JS `announced(prefix)` gains the same optional pattern filter on
  `Origin` and `Connection`; `@moq/room` reads the participant from the
  capture of each broadcast's exact announce, which pins it, and ignores
  overlap-only claims that carry `None`.
- Bindings, per the Cross-Package Sync checklist: `announced(prefix)`
  keeps its prefix and takes an optional filter (the moq-ffi `Pattern`
  type or its string form), and delivers events relative to the origin
  with `captures` on `MoqAnnounceUpdate`; `rs/libmoq`
  (`moq_announce_update` gains the captures under the existing string-out
  convention), the `py`, `swift`, `kt`, `dart`, and `go` wrappers,
  `doc/lib/*` for each, and `just test smoke-full`.
- `announced_broadcast(path)` keeps its shape; it is the literal case.

Public API: breaking on moq-net, @moq/net, moq-ffi, libmoq, and every
binding, so on dev. Wire: none beyond what the announce quest removes.

## Required

- [Announce event](/quest/m1/api-net-announce.md) - the prefix table and event shape this rebases onto

## Related

- [Origin narrowing](/quest/m2/origin-narrowing.md) - the runtime half that closes #2714 after the merge
- [Pattern grants](/quest/m2/path-patterns/interest.md) - the same scopes in the AUTH message
