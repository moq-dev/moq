# [XS] Delete SourceMap

## Goal

On `dev`, `moq_mux::SourceMap` and `Clock::source` are gone, so an `Anchor`
with its `Lane`s is the one way to map a source onto the broadcast clock.

## Plan

`SourceMap` is an `Anchor` plus one `Lane`, and nothing in the repository uses
it. A single-track source uses `Anchor` with one `Lane`. Migrate the
`SourceMap` tests in `rs/moq-mux/src/clock.rs` onto `Anchor`, and update
`doc/lib/rs/moq-mux.md`.
