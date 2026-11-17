# [S] One broadcast's failure never ends export archive for the others

## Goal

`moq export archive` records every broadcast independently: a broadcast
whose recording fails (a re-added track with another priority or timescale,
reused group sequences, a listing that ends in an error) is logged and
dropped, and every other broadcast keeps recording.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source.
`rs/moq-cli/src/archive.rs` returns the first recording's error, which drops
the task set and aborts every other recording. `doc/bin/cli.md` documents it
("Any recording that fails ends the export"), but AGENTS.md's Required rule
says a publisher that breaks its identity hurts only its own broadcast.

Recommended: ✅ log and drop the failed broadcast and keep exporting the rest;
the command still fails if the origin itself fails (rejected: keeping the
documented behavior, which lets one publisher end everyone's recording).
Update `doc/bin/cli.md`. Also check whether an ordinary publisher disconnect
surfaces as a listing error, which would make this hit on every dropped
session.

Public API: none (CLI behavior). Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#4034](https://github.com/moq-dev/moq/pull/4034) - added `export archive` with one failure ending every recording
