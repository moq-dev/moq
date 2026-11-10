# [S] rs/AGENTS.md's time-test rule matches the tree

## Goal

`rs/AGENTS.md` tells an agent writing a time-dependent Rust test what the
tree actually does. Its rule says such tests call `tokio::time::pause()` and
that only moq-net and moq-relay's in-memory tests use `#[kio::test]`; the
tree has about 4,900 `kio::test` uses (moq-mux, moq-tokio, moq-ffi, and
more) against about 200 `pause()` calls.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit, which
the maintainer prompted). Decided: ✅ fix the stale rule (rejected: leave it).

Follow `quest guide prompting`. State the rule once: the time-mocking rule
also appears in `quest context`, `REVIEW.md`, `rs/moq-net/AGENTS.md`, and
`clippy.toml`, so drop any copy that only repeats it. Say when `pause()` is
still the right tool, if ever.

Public API: none. Wire: none.
