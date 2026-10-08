# [M] JS per-track timelines

## Goal

`@moq/hang` publishes one timeline per track with the same records, cuts, and
catalog `archive` map as Rust.

## Plan

The read side landed with the archive line (#4034): the catalog schema parses
the `timelines` map and `Timeline.Consumer` reads one track's records. The
aligned multi-track producer was deleted rather than kept beside it, so JS
publishes no timeline today.

Port the Rust segmenter, producer, and enrollment
(`rs/moq-mux/src/timeline.rs`) to `js/hang/src/timeline.ts`, and have the
legacy container report its groups again. Cover the same cut rules and a
static catalog outliving other tracks' records, and check the records against
Rust output in the interop suite.

The JS `Timeline.Recorder` also gets the idle deadline and `flush()` that
[Idle tracks](/quest/m1/archive/flush.md) decides for Rust, so an idle browser
track's record closes within the same bound (Codex on #4301; moved here
2026-10-07 so the Rust flush does not wait on this port).

## Required

- [Timelines declare their segment duration](/quest/m1/archive/declared-duration.md) - the final `timelines` entry shape to port, so JS ports it once
