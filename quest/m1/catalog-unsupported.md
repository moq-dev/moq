# [S] A viewer sees that its player is too old for the catalog

## Goal

When `@moq/watch` reads a catalog whose version it does not support, the
broadcast reports a distinct status that says the player needs an update,
instead of going "offline" (fetched catalog) or staying "loading" (inline).

## Plan

After #5806, an unsupported catalog version is only logged. Decided
2026-10-17 (maintainer): add a distinct broadcast status rather than reusing
the error path, so the UI can tell a viewer to update instead of implying
the broadcast is down. Mirror the shape in Rust consumers' error so native
players can say the same.

Public API: a new broadcast status value in `@moq/watch`. Wire: none.
