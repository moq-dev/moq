# [XS] Import catalog finish

## Goal

`moq-cli`'s `import_delivers_the_catalog_finish_at_eof`
(`rs/moq-cli/tests/import.rs`) passes under a loaded `just check`. It runs
8.8 s alone against a 10 s timeout and fails with "announce timed out" under
load.

## Plan

Move it to a paused clock or an in-memory fixture, following this line's
rules; never raise the timeout.

Public API: none. Wire: none.
