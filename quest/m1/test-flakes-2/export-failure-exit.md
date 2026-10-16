# [S] moq-cli export exit tests hold under load

## Goal

`moq-cli`'s `an_export_failure_with_the_broadcast_up_exits_without_lingering`
(`rs/moq-cli/tests/export.rs`) passes under a loaded `just check`, fixed at
its cause. It still proves that a failed export exits 1 without waiting for
the broadcast to end. `a_drop_exits_one_once_the_linger_expires` in the same
file also passes under load, still proving an interrupted publisher's drop
makes the export exit 1 once the linger expires.

## Plan

Seen 2026-10-13 on #5458 and #5481 at load averages near 47 on 24 cores,
beside [auth serve signals](/quest/m1/test-flakes-2/auth-serve-signals.md):
it timed out and passed alone.

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

Folded in 2026-10-14 (maintainer): `a_drop_exits_one_once_the_linger_expires`
bounced #5528 from the merge queue, exiting 0 instead of 1. Its `interrupt`
helper (`rs/moq-cli/tests/export.rs:142`) sends SIGINT, sleeps 500 ms, then
closes the publisher's stdin, because the interrupted process only exits once
its stdin read returns. On a loaded runner stdin closed first, so the
publisher logged "input ended" and finished the broadcast cleanly instead of
dropping it. Fix the cause, not the sleep: preferably the publisher exits on
SIGINT without waiting for its stdin read, so the test never closes stdin.
#5510 landed that runtime-teardown fix on main, so drop the stdin close and
the sleep here.
`interrupted` (`export.rs:264`) also sleeps 1 s after the interrupt; replace
it with the exporter's paced tail draining, not the publisher's exit. Fixing
this race alone does not resolve the failed-export timing above.

Fixture cleanup on 2026-10-15: hold the publisher's stdin writer until its
signal-driven exit, and mark interrupted output only after the follower enters
linger and the stdout reader drains the queued tail. No scheduling sleep or
stdin EOF decides whether the publication ended cleanly. The test pins the
follower's log level for its lifecycle event.

The separate historical failed-export timing failure remains open. On current
main `0fc797747`, both named tests passed before these fixture changes under a
loaded host (about 32): failed-export exited in 2.285 s and the interrupted
case in 4.787 s. Those passes do not identify or resolve the historical cause.
Reproduce and record the failed bound and process before changing that arm.

Public API: none. Wire: none.

## Related

- [CLI stdin release](/quest/m1/cli-stdin-release.md) - the release backport of that fix
