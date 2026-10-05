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

Decided (2026-10-05): timedness is per track, in the shape [Typed
timedness](/quest/m1/typed-timedness.md) mirrors into `@moq/net`. Where the
2026-10-01 note above assumes a per-frame optional timestamp (an absent `at`
marking an untimed frame), that shape wins.

Public API: breaking. Wire: none.

## Required

- [@moq/net carries untimed frames faithfully](/quest/m1/js-untimed-model.md) - the model must hold an untimed payload before producers stop filling in now
