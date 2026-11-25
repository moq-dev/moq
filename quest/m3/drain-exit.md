# [S] A relay exits promptly once its drain deadline force-closes sessions

## Goal

A `moq-relay` whose drain deadline force-closes the sessions still open exits
within a bounded, small time after it logs `drain deadline force-closed
sessions; exiting` (`Observer::drained`, `rs/moq-relay/src/shutdown.rs`). Twice in
the mesh replay, a relay logged that line and was still running 20 s later, so
the replay counted it `hung` and killed it. Under systemd it would run until its
stop timeout and then be killed, slowing every restart by that timeout.

## Plan

Reproduce first: a drained relay holding a session that ignores the GOAWAY until
the deadline, as `rs/moq-relay/tests/integration/drain_dialed.rs` sets up for
dialed links, and check that the process exits. The mesh replay's `hung`
check (`test/mesh-replay/README.md`) is the second signal.

After that log line, `Relay::run` (`rs/moq-relay/src/relay.rs`) still awaits
`cluster.close()`, `server.close()`, the peers listener's `close()`, and the QUIC
and io_uring workers' `shutdown()`, and `main` then drops the Tokio runtime,
which waits for blocking tasks. One of those waiting on a force-closed
connection, or on a task nothing cancels, would hold the exit. Find which one
from the hung relay (a stack dump, or a log line after each step), and fix that
step so it ends once the deadline has passed, rather than capping the whole
shutdown with a timeout.

The earlier fix,
[moq-dev/moq#9213](https://github.com/moq-dev/moq/pull/9213) (`3728b964d2`),
made a draining relay exit once nothing reads through its dialed links; this is
the path where the deadline fires first.

Decided 2026-11-25 (maintainer, `/quest-plan`): ✅ keep (rejected: drop).

## Related

- [moq-dev/moq#8596](https://github.com/moq-dev/moq/pull/8596) - the restart beside a draining relay, whose old process this keeps alive
- [A new relay takes over from a draining one](/quest/m0/test-flakes-4/relay-handoff-drain.md) - a drain test flake on the same path
