# [XS] Iroh secret written private

## Goal

A generated iroh secret key file is created with mode 0600, never through an
existing file or symlink, and a pre-existing key that is not a regular file
or is readable by group or others is refused.

## Plan

The implementation creates files exclusively with Unix mode 0600 and validates
existing opened files before reading. Its tests live in `moq-tokio`'s `secret`
module so they run without the optional `iroh` feature.

Remaining: run the focused persisted-secret tests and `just ci` through
`quest gate`; host load prevented either command from starting. Resolve any
findings, then remove this quest and its links and mark the PR ready for review.

From the 2026-10-27 audit (RELAY-3), repeated on 2026-10-28 with the additional
regular-file requirement. The `iroh` feature is off by default in moq-relay.

Public API: none. Wire: none.
