# [M] Bindings start a group at a given timestamp

## Goal

moq-ffi's `MoqTrackProducer::append_group()` and moq-c's `track_group` take a
required `Timestamp` start, the way Rust's `track::Producer::append_group`
does, in every binding, so no binding invents "one past the newest start".

## Plan

[moq-dev/moq#8915](https://github.com/moq-dev/moq/pull/8915) renamed every
binding's `sequence` to `start`; this is what it left. Rust's
`track::Producer::append_group` takes a required `Timestamp` start
([moq-dev/moq#8961](https://github.com/moq-dev/moq/pull/8961)), with no
implicit "one past the previous start". moq-ffi's `append_group()`
(`rs/moq-ffi/src/producer.rs`) and moq-c's `track_group`
(`rs/moq-c/src/publish.rs`) still take no start and compute one past the
newest start in the track's timescale, each with a private `next_start`.

Recommended (kept by the maintainer in #8961): give both a required timestamp
start, the `{value, scale}` record the bindings already pass, in one breaking
release, and drop both `next_start`s. `create_group(start)` already takes an
explicit start, so the choice is whether a binding keeps a start-less
convenience at all. Then update every wrapper (Python, Go, Swift, Kotlin,
Dart, C++), the tests and benches that call `append_group()`, and each
`doc/lib` page, and list the rename in the PR's Impact for each package's
`MIGRATE.md`. Check the group rename once more: a name left behind is a
second breaking release.

Public API: breaking in every binding. Wire: none.
