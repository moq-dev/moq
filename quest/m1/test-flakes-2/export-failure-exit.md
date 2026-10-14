# [S] moq-cli failed-export exit test holds under load

## Goal

`moq-cli`'s `an_export_failure_with_the_broadcast_up_exits_without_lingering`
(`rs/moq-cli/tests/export.rs`) passes under a loaded `just check`, fixed at
its cause. It still proves that a failed export exits 1 without waiting for
the broadcast to end.

## Plan

Seen 2026-10-13 on #5458 and #5481 at load averages near 47 on 24 cores,
beside [auth serve signals](/quest/m1/test-flakes-2/auth-serve-signals.md):
it timed out and passed alone.
`the_same_instance_within_the_linger_continues` failed beside it but is
tracked by [TS same-instance resume](/quest/m1/ts-same-instance-resume.md).

The test runs an in-process relay with real `moq import` and `moq export`
processes. Two bounds apply. The exit wait has a 30 s `TIMEOUT`, so a child
that lingers fails there. The "exited within 10 s" assertion runs only after
the child exits, so a slow but completed exit fails there. That 10 s is the
property (no lingering) and must stay bounded. It starts after both `spawn()`
calls return, just before the AV1 write, so it excludes the parent's spawn
but still covers each child's own startup and relay connection, the import,
the export's catalog read, and its exit. Cleanup then closes the import's
stdin and waits on it with the same 30 s guard, which fails with the same
"moq never exited" message.

Reproduce under load first and record which bound fired, and for "moq never
exited" which process. Find what in that
interval is slow under load and fix it, or move the check to a deterministic
in-process test if the process boundary isn't what it proves. Never widen a
timeout or add a retry.

Public API: none. Wire: none.
