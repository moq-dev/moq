# [M] A panic inside moq-tokio's connection internals fails callers instead of panicking them

## Goal

After a moq-tokio QUIC connection driver panics, every client-side caller
(`Connecting`, the connection, its streams) gets the connection's terminal
error instead of panicking in its own task, and the connection still closes
to the peer when its last handle drops. Two background tasks
[moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360) left out are
supervised by its rule: a panic in a listener's certificate reload watcher
(`tls::Reload`) fails `Listener::accept` like a dead endpoint, and a panic
in a qmux session reader/writer or WebSocket pump ends that session with an
error and is counted.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360),
which finishes [task supervision](/quest/m2/task-supervision.md).

[moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360) wakes a dead driver's waiters and isolates each handshake in
`Server`'s shared accept queue, but keeps the connection mutex fail-fast
(`rs/moq-tokio/src/quic/driver/mutex.rs`, `lock()` unwraps). So on the
server every accessor runs isolated, while a client's `Connecting` or
stream call that touches the poisoned lock panics in the application's task;
each caller would have to isolate it. And `ConnectionRef::drop`
(`rs/moq-tokio/src/quic/driver/connection.rs:776`) skips the implicit close
when the lock is poisoned, so the peer waits for its idle timeout.

Open for the implementing PR (recommendation first):

1. Record the panic as the connection's terminal error once, where the
   driver's isolation catches it, and have every accessor read that error
   instead of the poisoned state. [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360) tried per-accessor poison recovery
   ([34c7666147](https://github.com/moq-dev/moq/commit/34c7666147)) and reverted it because later handshake steps stayed
   exposed; a single terminal error checked before locking avoids that.
2. Keep fail-fast and document that client callers must isolate.

For the drop: send the close from the terminal-error path (the driver's
isolation) rather than from the last handle, so a poisoned lock doesn't
matter.

`tls::Reload` is in `rs/moq-tokio/src/tls.rs:3332`; the qmux tasks are
the plain `tokio::spawn`s at `rs/qmux/src/session.rs:486-487` and
`rs/qmux/src/transport.rs:188` and `:985`. qmux can't reach
`moq_tokio::panic`, so either qmux gets a small isolate of its own or its
caller in moq-tokio observes the handles. Add a regression per class that
injects a panic, as [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360)'s `driver_panic_fails_connecting` does.

Public API: none expected (behavior only); a new `Error` variant for a
panicked driver if one is needed. Wire: none.

## Required

- [The remaining long-lived tasks are supervised](/quest/m2/task-supervision.md) - its PR, [moq-dev/moq#7360](https://github.com/moq-dev/moq/pull/7360), adds the isolation this extends
