# [S] One owner logs every relay connection's lifecycle

## Goal

Every relay accept path, the io_uring workers included, logs a connection's
`negotiated` and `connection closed` lines under the `moq_relay::connection`
target inside its `conn{id=N}` span, from one place. A connection that is
refused, fails its handshake, or panics still logs its close. The Interop
harness's accounting (`test/interop/relay-log.sh`), which filters to that
target and fails a round whose opened connection never closes, then works
unchanged on a relay started with `--runtime-io-uring`.

## Plan

From #5602, which moved the close log into `moq_relay::Connection::run`
(`rs/moq-relay/src/connection.rs`) so QUIC and WebSocket sessions pair their
span with their close under one filter. Checked on main at `de6c27302`: the
io_uring accept path (`serve_connection` in `rs/moq-relay/src/uring.rs`)
does not go through `Connection::run`. It logs `negotiated` and
`connection closed` under `moq_relay::uring`, with the id as a field and no
`conn` span. A panic in its supervise task is isolated and dropped without a
close line. Interop does not run the ring today, so nothing fails yet.

Root problem: the ring path re-implements the connection lifecycle that
`Connection` owns: admission (the LAN credential branch, auth on the shared
runtime, refusal recording), limits, `lease.preparing`, supervision, and the
logs around them. The logs are only the visible drift. The copy exists
because the handshake and session driver must stay on the `!Send` worker
while admission and supervision run on the shared runtime.

Recommendations:

- Make one owner of the lifecycle that both accept paths call, so the span,
  the id, the `negotiated` line, and the close (including a panic) cannot
  drift again. Prefer sharing admission and supervision with `Connection`
  over a logging helper that only hides the duplication; if part of the
  admission cannot be shared across the worker boundary, record why.
- The ring connection spans two runtimes, so the `conn` span has to follow
  both its worker future and its shared-runtime task.
- Regressions, logged with only the `moq_relay::connection` target as the
  harness filters:
  - An io_uring session shows its `conn` span and its close, like
    `relay_websocket_logs_its_close_under_a_cluster_id` in
    `rs/moq-relay/tests/integration/smoke.rs`. It belongs in `tests/integration/runtime_uring.rs`,
    which skips loudly below the kernel floor.
  - A panic inside the lifecycle owner, forced on the ring's
    shared-runtime supervision, still logs that connection's close. This is
    the concrete missing close today: `serve_connection` isolates the
    supervise task's panic and drops the result. It must fail without the fix.

Public API: none expected. Wire: none.

## Related

- [IETF on the ring](/quest/m2/uring-ietf.md) - also touches the ring's accept path
- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - ships the ring, whose operators read these logs
