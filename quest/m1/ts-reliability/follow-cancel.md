# [S] A stitched export survives a cancelled replacement

## Goal

A `moq export ts --stitch --follow` whose follow window has run out, and whose
pending replacement is then cancelled, ends cleanly. It never hits the
follower's "selected catalog" panic.

## Plan

Found 2026-10-28 while fixing [moq-dev/moq#7137](https://github.com/moq-dev/moq/pull/7137),
untested. When the follow window runs out, the old program's catalog is
dropped. The follower reads that catalog when it subscribes to a returning
broadcast, so a replacement cancelled after that point may reach the same
panic #7137's first attempt hit.

Decided 2026-10-28 (maintainer): write the deterministic test first, in
`rs/moq-mux/src/container/ts/export_test.rs`. If it panics, fix it at the cause
in `export.rs`. If it doesn't, the quest lands the test alone and says so.

Public API: none. Wire: none.
