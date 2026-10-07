# [M] JS publishing never invents a timestamp

## Goal

`@moq/json`, `@moq/flate`, and `@moq/net` publish APIs never fill in
`Timestamp.now()` for the caller, mirroring
[Publishing never invents a timestamp](/quest/m1/publish-timestamp.md). A
payload published without a timestamp goes out untimed.

## Plan

Today the json and flate snapshot and stream producers default
`at = Timestamp.now()`, the json window producer always uses now, and js/net's
`writeString`, `writeJson`, and `writeBool` stamp now.

Decided (2026-10-01): `@moq/net` exports `Timed<T>`
(`{ value: T; at?: Time.Timestamp }`), the Rust name and shape. Each producer
takes a `Timed<T>`; an absent `at` is untimed, never now. The consumers return
the same type in
[JS data consumer timestamps](/quest/m1/js-data-consumer-timestamps.md).
Callers in the repository (js/hang catalog, js/publish, js/room) pass their
clock's now explicitly. Update `doc/lib/js/{json,flate,net}.md`.

Decided (2026-10-05, types settled 2026-10-06): timedness is per track, as
the [untimed model](/quest/m1/untimed-model.md) decided and `@moq/net`
mirrors: `timescale` is optional, frames keep an optional timestamp, and a
frame whose timedness doesn't match its track is refused. An absent
`at` therefore belongs on an untimed track.

Public API: breaking. Wire: none.

## Required

- [@moq/net carries untimed frames faithfully](/quest/m1/js-untimed-model.md) - the model must hold an untimed payload before producers stop filling in now
