# [M] JS publishing never invents a timestamp

## Goal

`@moq/json`, `@moq/binary`, and `@moq/net` publish APIs never fill in
`Timestamp.now()` for the caller, mirroring
[Publishing never invents a timestamp](/quest/m1/publish-timestamp.md). A
payload published without a timestamp goes out untimed.

## Plan

Today the json and binary snapshot and stream producers default
`at = Timestamp.now()`, the json window producer always uses now, and js/net's
`writeString`, `writeJson`, and `writeBool` stamp now.

Decided (2026-10-01): `@moq/net` exports `Timed<T>`
(`{ value: T; at?: Time.Timestamp }`), the Rust name and shape. Each producer
takes a `Timed<T>`; an absent `at` is untimed, never now. The consumers return
the same type in
[JS data consumer timestamps](/quest/m1/js-data-consumer-timestamps.md).
Callers in the repository (js/hang catalog, js/publish, js/room) pass their
clock's now explicitly. Update `doc/lib/js/{json,binary,net}.md`.

Public API: breaking, on `dev`. Wire: none.

## Required

- [Plan: untimed objects](/quest/m1/plan-untimed-objects.md) - an untimed payload must travel as untimed before producers stop filling in now
