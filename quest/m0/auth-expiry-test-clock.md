# [S] Make the auth expiry test independent of wall-clock scheduling

## Goal

`rs/moq-relay/src/auth.rs::tests::expired_resolves_at_credential_expiry`
reliably verifies credential expiry under workspace test load.

## Plan

The test sets a deadline from `SystemTime::now() + 100ms`, then asserts that
at least 100ms passed on Tokio's paused clock. `elapsed` reads `SystemTime`
again when awaited, so real time between those reads reduces the remaining
sleep without advancing the paused clock. PR #3446 observed the resulting
"resolved before expiry" failure; the two clock reads remain in the code.

Use a controlled clock boundary or test the remaining deadline explicitly.
Preserve coverage that credentials neither expire early nor remain authorized
after their deadline. Add a deterministic regression with elapsed wall time
between deadline creation and waiting; do not widen a timeout or add retries.
