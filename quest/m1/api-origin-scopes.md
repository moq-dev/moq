# [M] Bindings report announce matches

## Goal

Every binding that exposes announcements takes a pattern scope and reports
the match like Rust and JavaScript. `moq-ffi`'s
`MoqOriginConsumer::announced(prefix)` scopes by a literal root today, so its
only wildcard is the implicit trailing `**`.

Rust and JavaScript pattern scopes and captures landed in
[#3746](https://github.com/moq-dev/moq/pull/3746). This quest tracks only the
cross-language binding mirror that PR explicitly left unfinished.

## Plan

- Bindings, per the Cross-Package Sync checklist: `announced(prefix)`
  keeps its prefix and takes an optional filter (the moq-ffi `Pattern`
  type or its string form), and delivers events relative to the origin
  with `captures` on `MoqAnnounceUpdate`; `rs/libmoq`
  (`moq_announce_update` gains the captures under the existing string-out
  convention), the `py`, `swift`, `kt`, `dart`, and `go` wrappers,
  `doc/lib/*` for each, and `just test smoke-full`.
- `announced_broadcast(path)` keeps its shape; it is the literal case.

Public API: breaking on moq-ffi, libmoq, and every binding, so on dev.
Wire: none.

## Related

- [Origin narrowing](/quest/m2/origin-narrowing.md) - the runtime half that closes #2714 after the merge
- [Pattern grants](/quest/m2/path-patterns/interest.md) - the same scopes in the AUTH message
