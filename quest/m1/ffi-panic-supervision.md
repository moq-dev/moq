# [M] A panic in a binding's background task reaches the host

## Goal

A panic in any task moq-c or moq-ffi spawns (consume, publish, origin, audio,
video, server, and `ffi::spawn`'s tasks, including the origin driver)
reaches the host as a panic error through that object's final callback or
the uniffi call, never as a lost callback or `Cancel`. A moq-tokio driver
panic under a binding reports `Panic`, not `Cancel`.

## Plan

Follow-up from [#5447](https://github.com/moq-dev/moq/pull/5447), which made
a panic close only its session and supervised moq-c's session tasks. The
other moq-c tasks still drop a panic, so the host's final callback never
fires and a C host waits forever; moq-ffi's `ffi::spawn` tasks do the same
for uniffi callers.

Decided 2026-10-13 (maintainer): m1, one quest for both bindings. Supervise
each task's `JoinHandle` the way #5447 did for moq-c sessions, reusing
`moq_tokio::panic`. Carry a driver's panic through as a panic rather than
the `Cancel` its dropped channel produces today. Add a regression per
binding that injects a panic into one background task and asserts the
host sees `MOQ_ERROR_PANIC` (moq-c) or the binding's panic error (moq-ffi),
while another object keeps working. A binding's origin task needs its own owner for
the driver, so its panic can be reported; wrapping the helper that aborts the
process (`moq_tokio::origin::spawn`) is not enough. Also add the regression #5447 left out:
a moq-tokio accept loop that panics ends its listener.

Public API: none expected, beyond the panic error reaching more callbacks.
That is a host-visible change even without a new error variant, so walk the
`rs/moq-ffi` row of the Cross-Package Sync table: every wrapper and
`doc/lib` page surfaces the panic error. Wire: none.

## Related

- [Long-lived task supervision](/quest/m2/task-supervision.md) - the remaining process-wide tasks
