# [S] moq-cli process tests hold under load

## Goal

`moq-cli`'s `auth_serve_stops_on_sigint`, `auth_serve_stops_on_sigterm`
(`rs/moq-cli/tests/auth_serve.rs`) and
`an_export_failure_with_the_broadcast_up_exits_without_lingering`
(`rs/moq-cli/tests/export.rs`) pass under a loaded `just check`, fixed at
their cause. Each still proves what it does today: a signal exits `auth
serve` cleanly, and a failed export exits 1 without waiting for the broadcast
to end.

## Plan

Seen 2026-10-13 on #5458 and #5481 at load averages near 47 on 24 cores:
each timed out and passed alone. The tests spawn real `moq` processes and
bound them with wall-clock deadlines (`TIMEOUT` of 10 s, and the export's
"exited within 10 s" check), so a slow spawn under load reads as a failure.
`the_same_instance_within_the_linger_continues` failed beside them but is
tracked by [TS same-instance resume](/quest/m1/ts-same-instance-resume.md).

Reproduce under load first. Then assert on an event instead of a deadline
where the deadline isn't the property under test: the auth tests already wait
for "auth server listening", so their exit wait can follow the same pattern.
The export test's 10 s check is the property (no lingering), so measure it
from an event the process logs rather than from the parent's spawn. If the
process itself is slow under load, fix that. Never widen a timeout or add a
retry.

Public API: none. Wire: none.
