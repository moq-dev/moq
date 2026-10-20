# [S] moq-tokio redial tests hold up under load

## Goal

The moq-tokio tests `a_failing_redial_reports_why_until_it_connects` and
`a_fleet_drain_redials_through_a_fresh_resolve` (`rs/moq-tokio/src/connection.rs`)
pass reliably under a loaded `just check` and in CI.

## Plan

Seen in [CI](https://github.com/moq-dev/moq/actions/runs/39097647984/job/117587545922).
Locally they failed 5 of 240 runs under load, with several different messages.

Suspected cause: the first test binds a port, drops it, and assumes nothing
listens there afterwards. Under parallel load another process takes the port,
the same pattern [#5873](https://github.com/moq-dev/moq/pull/5873) fixed in
the GStreamer tests. The 10 s deadlines add to it.

- Remove the port-reuse assumption, for example with a listener that refuses
  connections or a socket the test keeps holding.
- Assert on events instead of 10 s deadlines.
- Per the epic's rules, a paused tokio clock fights real sockets, so mocking
  time alone is not the fix.
- Confirm the second test's cause on its own; it may share the fixture or
  fail for a reason of its own.

Checkpoint 2026-10-19: the first test's released-port assumption reproduced
without load by rebinding its port before the connect. Its fixture now holds
a listener that hangs up each accepted connection, and the test waits on the
connection's existing state notifications instead of polling with sleeps.
The regression also checks that another listener cannot bind its port.

The fleet test remains unfinished: its listeners stay alive, and the CI job
linked above currently reports both named tests passing. No independent cause
has been established for that test. Keep this quest until its failure is
reproduced and fixed, and loaded `just check` plus CI establish reliability.

Public API: none. Wire: none.
