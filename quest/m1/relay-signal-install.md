# [M] The relay catches shutdown signals before it serves

## Goal

A SIGTERM or SIGINT that reaches `moq-relay` at any point after it starts
serving drains it with a GOAWAY instead of killing it. An embedder whose
startup fails can still be stopped by its own signals, and a second signal
during a drain still exits at once.

## Plan

Split from the drain cleanup work (the `moq` CLI already builds its `Signals`
at the top of `run_stages`, before anything serves, since #5575). The relay
still installs its handlers lazily, on `drain`'s first poll in `Relay::run`
(`rs/moq-relay/src/relay.rs`). That poll comes after the io_uring and QUIC
workers start serving and after READY, so a SIGTERM in that window kills the
relay with no GOAWAY. #5575 tried moving the relay's registration and backed
out: each placement broke another case, and these need to be solved together:

- Embedder handler leak: tokio never uninstalls a handler. Registering
  before a step that can fail (the admissions check, `cluster.start()`,
  `uring.serve`) leaves an embedder that recovers from the error ignoring
  SIGINT and SIGTERM.
- io_uring worker startup: `uring.serve` both serves and can fail, since its
  first worker accepts while later ones start. Handlers installed before it
  leak on failure, and handlers installed after it race the first worker.
- Signals during startup: a signal queued before READY should skip READY
  and drain. A synchronous probe before READY can miss one that the IO driver
  has not processed yet (for example, while `uring.serve` blocks).
- Queued second request: if startup consumes a signal, or the embedder has
  already fired the trigger, a further queued signal must still be the
  second request and exit at once. `drain`'s first wait must not take it as
  another first signal; bias it toward a drain that has already started.
- The CLI reports ready from many stage paths (`notify_ready`) and checks no
  stop state, so a signal during its startup drains right after READY.

Prefer one signal owner shared by the relay and the CLI, as their drain now
is (`moq_relay::shutdown::Observer::drained`). Test each case above with a
raised signal, holding the process-wide signal lock the existing tests use.

Public API: possibly how `Relay::with_signals` installs its handlers.
Wire: none.
