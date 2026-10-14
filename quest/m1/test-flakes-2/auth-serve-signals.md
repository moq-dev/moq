# [S] moq-cli auth serve signal tests hold under load

## Goal

`moq-cli`'s `auth_serve_stops_on_sigint` and `auth_serve_stops_on_sigterm`
(`rs/moq-cli/tests/auth_serve.rs`) pass under a loaded `just check`, fixed at
their cause. They still prove that a signal exits `auth serve` cleanly.

## Plan

Seen 2026-10-13 on #5458 and #5481 at load averages near 47 on 24 cores,
beside [export failure exit](/quest/m1/test-flakes-2/export-failure-exit.md):
each timed out and passed alone. Each test spawns a real `moq auth serve` and
bounds two waits with a 10 s `TIMEOUT`: the "auth server listening" log line,
which only gates setup, and the exit after the signal, which is the property.

Reproduce under load first and record which wait expired. The exit wait must
stay bounded so an ignored signal fails at its assertion instead of hanging
CI. Find what in the expired interval is slow under load and fix it, or move
the check to a deterministic in-process test if the process boundary isn't
what it proves. Never widen a timeout or add a retry.

Public API: none. Wire: none.
