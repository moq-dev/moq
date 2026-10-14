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
"exited within 10 s" check), so a slow process under load reads as a failure.
`the_same_instance_within_the_linger_continues` failed beside them but is
tracked by [TS same-instance resume](/quest/m1/ts-same-instance-resume.md).

Reproduce under load first and record which wait expired. Some deadlines are
the property under test and must stay bounded, so a regression fails at its
assertion instead of hanging CI: the auth tests' exit wait after the signal,
and the export's 10 s check. The auth tests' wait for "auth server listening"
only gates setup. The export check starts after both spawns, just before the
AV1 write, so it covers the import, the export's catalog read, and its exit,
not process startup. Find what in the expired interval is slow under load and
fix it, or move a check to a deterministic in-process test if the process
boundary isn't what it proves. Never widen a timeout or add a retry.

Public API: none. Wire: none.
