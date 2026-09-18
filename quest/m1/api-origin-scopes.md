# [M] Bindings report the announce match

## Goal

Every binding that exposes announcements takes a pattern scope and reports
the announce match the way Rust and JS do: the covered pattern relative to
the origin, plus one capture per wildcard in the scope. `moq-ffi`'s
`MoqOriginConsumer::announced(prefix)` (`rs/moq-ffi/src/origin.rs`) scopes
by a literal root today, so its only wildcard is the implicit trailing `**`
and a capture there would always equal the pattern. The announce event shape
is a published API in every language, so it settles on dev before the
release.

## Plan

- `announced(scope)` takes a pattern (the moq-ffi `Pattern` type, or its
  string form parsed with the same rules as JS), refuses nothing the origin
  accepts, and delivers events relative to the origin with `captures` on
  `MoqAnnounceUpdate`, mirroring `AnnounceUpdate.captures` in Rust and
  `Announce.Update.captures` in JS.
- Follow the Cross-Package Sync checklist: `rs/libmoq` (`moq_announce_update`
  gains the captures; a C caller reads them with the existing string-out
  convention), the `py`, `swift`, `kt`, `dart`, and `go` wrappers, `doc/lib/*`
  for each, and `just test smoke-full`.
- `announced_broadcast(path)` keeps its shape; it is the literal case.

Public API: breaking on moq-ffi, libmoq, and every binding, so on dev.
Wire: none.

## Related

- [Origin narrowing](/quest/m2/origin-narrowing.md) - the runtime half that closes #2714 after the merge
- [Pattern interest](/quest/m2/path-patterns/interest.md) - carries the same scopes on the lite-06 wire
