# [L] A panic closes only its connection

## Goal

A panic inside one connection or session closes that connection and leaves
the process running: the relay, the CLI, and every FFI host (a library panic
reaches the host as `MOQ_ERROR_PANIC` or a binding exception). A panic in a
long-lived process task still exits the process, so nothing becomes a silent
zombie.

## Plan

Found from [#5407](https://github.com/moq-dev/moq/issues/5407): the
workspace's `panic = "abort"` (dev and release profiles, added in #804) makes
moq-c's `catch_unwind` (`rs/moq-c/src/ffi.rs`) and uniffi's `rust_call`
dead in shipped builds, so any library panic kills the host app.

#804 chose abort because a rustls panic in a detached HTTPS task left the
relay up but serving nothing. That bug is gone (the provider is installed at
startup and the HTTPS config is built before binding), but the failure mode
returns for any detached long-lived task under unwind.

Decided 2026-10-13 (maintainer):

- Unwind workspace-wide: delete `panic = "abort"` from both profiles. Rejected:
  a separate `ffi` profile for binding builds, since relay connections need
  isolation too. wasm needs no override (stable std aborts there anyway).
  Measure the moq-ffi staticlib against the Go-mirror size ceiling noted in
  `Cargo.toml`.
- Ignore poison. kio's `Lock::lock` (`rs/kio/src/lock.rs`) panics on a
  poisoned mutex, so one panic under a shared origin/broadcast/track lock
  would cascade to every session, or double-panic into an abort in
  `Producer::drop`. Use `unwrap_or_else(PoisonError::into_inner)` there
  (loom-compatible) and at the cross-session std locks (moq-net cache, stats,
  resume, frame, time; the relay; moq-c `STATE`; moq-ffi polls). Per-session
  locks may keep panicking. Rejected: poison closing the channel (more kio
  code), and parking_lot (no loom).
- Supervise every task. Every spawned task's `JoinHandle` is observed:
  - a per-connection task's panic is logged (`tracing::error!` with the
    connection span, a panic counter on `/metrics`) and ends only that
    connection; dropping its driver already sends CONNECTION_CLOSE and
    retracts its broadcasts;
  - a long-lived task's panic propagates out of `Relay::run` (or the CLI's
    main) and exits the process: the origin driver
    (`moq_tokio::origin::spawn_config`), cache governor, HTTPS cert reload,
    and the noq endpoint driver;
  - a cluster dial task is per peer, so it counts as a connection, and its
    peer is re-dialed instead of swallowed by `join_next`.
  Rejected: a panic hook that aborts unless a task-local "isolated" flag is
  set (catches tasks nobody listed, but hides policy in a hook), and
  log-only (reintroduces #804).
- moq-c: await each session task's inner handle and report `MOQ_ERROR_PANIC`
  through its terminal callback, so the entry doesn't leak.
- Regression test: a fake transport whose poll panics. Only that session
  closes, its broadcasts leave the origin, and a second session keeps
  working.
- Regression test for the fatal side: inject a deterministic panic into one
  named long-lived task (such as the cache governor) and assert
  `Relay::run` returns with the panic instead of logging and continuing.
- Update comments and docs that assume abort inline, including
  `doc/lib/cpp/index.md`, and document the panic counter's name and meaning
  in `doc/bin/relay/http.md`.

Public API: none in Rust. Operational: a new relay `/metrics` panic counter.
Wire: none.

## Related

- [Unset host clock](/quest/m0/wall-clock-omit.md) - removes the panic that surfaced this
- [Fuzz builds under LTO](/quest/m0/fuzz-lto.md) - also edits the release profile
