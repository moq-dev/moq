# [M] JS publishing requires a timestamp

## Goal

`@moq/json`, `@moq/binary`, and `@moq/net` publish APIs take a timestamp and
never fill in `Timestamp.now()` for the caller, mirroring
[Publishing requires a timestamp](/quest/m1/publish-timestamp.md).

## Plan

Today the json and binary snapshot and stream producers default
`at = Timestamp.now()`, the json window producer always uses now, and js/net's
`writeString`, `writeJson`, and `writeBool` stamp now. Make the timestamp a
required argument on each, with the same shape as Rust's `Timed`. Callers in
the repository (js/hang catalog, js/publish, js/room) pass their clock's now
explicitly. Update `doc/lib/js/{json,binary,net}.md`.

Public API: breaking, on `dev`. Wire: none.
