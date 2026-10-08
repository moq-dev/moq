# [M] JS publishing never invents a timestamp

## Goal

`@moq/json`, `@moq/flate`, and `@moq/net` publish APIs never fill in
`Timestamp.now()` for the caller, mirroring
[Publishing never invents a timestamp](/quest/m1/publish-timestamp.md). A
payload published without a timestamp goes out untimed.

## Plan

The remaining work is these defaults (checked 2026-10-08): the json and
flate snapshot and stream producers default `at = Timestamp.now()`, the json
window producer always uses now, and js/net's `writeString`, `writeJson`, and
`writeBool` (on the group and track producers) stamp now, as does `zod.write`
through `writeJson`.

Decided 2026-10-08: delete `writeString`, `writeJson`, and `writeBool`
rather than re-typing them to take a timestamp; `writeFrame` already takes
one. `zod.write` takes the timestamp or goes with them. Their callers
(`js/clock`, `js/net/examples/publish.ts`) write frames with their clock's
now explicitly.

Decided (2026-10-01): `@moq/net` exports `Timed<T>`
(`{ value: T; at?: Time.Timestamp }`), the Rust name and shape. Each producer
takes a `Timed<T>`; an absent `at` is untimed, never now. The consumers return
the same type in
[JS data consumer timestamps](/quest/m1/js-data-consumer-timestamps.md).
Callers in the repository (js/hang catalog, js/publish, js/room, js/clock)
pass their clock's now explicitly. Update `doc/lib/js/{json,flate,net}.md`.

Decided (2026-10-05, types settled 2026-10-06): timedness is per track, as
the untimed model ([#4822](https://github.com/moq-dev/moq/pull/4822)) decided and `@moq/net`
mirrors: `timescale` is optional, frames keep an optional timestamp, and a
frame whose timedness doesn't match its track is refused. An absent
`at` therefore belongs on an untimed track. `@moq/net` already refuses a
mismatched frame, so the group helpers, which still stamp now, only work on a
timed track today.

Decided (2026-10-07): `@moq/net` has no default timescale. Omitting
`Track.Info.timescale` declares an untimed track, so every timed publisher
names its units, as Rust will once
[An undeclared Rust timescale means untimed](/quest/m1/rust-untimed-default.md)
lands.

Public API: breaking. Wire: none.
