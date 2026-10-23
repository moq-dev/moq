# [S] Publication paths refuse empty segments

## Goal

A publication path with an empty segment, such as `room//cam`, is refused
in Rust and JS instead of being silently collapsed to `room/cam`.

## Plan

[moq-dev/moq#6441](https://github.com/moq-dev/moq/pull/6441) refuses `.` and
`..` segments. `Path` normalization (`rs/moq-net/src/path/mod.rs`) still
collapses empty ones, so two different inputs name the same broadcast.

Decided 2026-10-23 (maintainer): fail loud. Refuse empty publication segments
before normalization, at the same point #6441 refuses navigation segments,
in Rust and `@moq/net`, with tests for leading, trailing, and doubled
slashes. Start after #6441 merges.

Public API: inputs that were accepted now fail. Wire: none.
