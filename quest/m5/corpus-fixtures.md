# [XS] Shared corpora live in each crate's fixtures/

## Goal

Every JSON corpus a Rust crate and a JS package both replay, except the
audio jitter corpora under `doc/concept/`, lives in that crate's `fixtures/`
directory, so a contributor finds them in one place.
`rs/moq-auth/tests/corpus.json` moves to `rs/moq-auth/fixtures/`, matching
`rs/moq-net/fixtures/lite.json`, `rs/hang/fixtures/`, and
`rs/moq-msf/fixtures/`.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up).
Moved to m5 2026-11-12 (maintainer, /quest-audit): no consumer; parked rather than deleted.

- Move the file and update its readers: `rs/moq-auth/src/corpus.rs`
  (`include_str!` and the module doc) and `js/auth/src/corpus.test.ts`.
- `tests/` keeps only integration test targets (`released_token.rs`).
- The audio jitter corpora under `doc/concept/audio-jitter/` stay: the spec
  document cites them as its own conformance data, not one crate's.

Public API: none. Wire: none.
