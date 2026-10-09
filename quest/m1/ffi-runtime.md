# [S] FFI apps run on more than one thread

## Goal

moq-ffi drives moq on a multi-thread tokio runtime instead of one
`current_thread` runtime thread, so noq's endpoint and connection drivers
run beside a busy session, and Go, Python, Swift, Kotlin, and Dart apps get
the parallelism their own runtimes already have.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview (paper trail in the PR that
added this quest):

- Today `runtime()` in `rs/moq-ffi/src/ffi.rs` builds
  `new_current_thread().enable_all()` on one dedicated `moq-ffi` thread.
  Everything shares it: noq's
  endpoint and connection drivers, the moq-tokio reconnect loop, the session
  driver, and the origin driver. The pattern was copied from libmoq (#732,
  #1071) with no stated reason.
- Use `new_multi_thread().enable_all()` with tokio's default worker count.
  No configuration knob until an embedder needs one. Rejected: a small fixed
  pool, and keeping `current_thread` behind the
  [serve budget](/quest/m0/serve-budget.md) alone.
- Keep the "foreign caller's thread never drives our futures" property: work
  still runs via `Task::run` and `detached`. Check the exported async fns that
  await in place (`subscribe_catalog`, `subscribe_track`, `fetch_group`,
  `request_broadcast`, `decode_audio`, `decode_video`) and the sync methods
  that enter the runtime context (`MoqAudioProducer::write`).
- Callbacks change contract. Today moq-c status callbacks and uniffi
  callbacks fire one at a time on the single runtime thread; on workers they
  fire on any thread, and callbacks for different handles run at once.
  Document that in `doc/lib/` and audit binding code that assumes serialized
  callbacks.
- Shutdown keeps its meaning: when `moq_ffi_shutdown` returns,
  no task runs and no callback into the host is in flight, so Python's
  `atexit` and JVM exit stay clean. `shutdown_background()` no longer gives
  that with N workers; the shutdown must wait for them.
- Decided 2026-10-08: moq-ffi only. The
  [generated C bindings](/quest/m1/c/README.md) sit on moq-ffi and inherit
  it; the hand-written `rs/moq-c` stays untouched until it is
  [retired](/quest/m1/c/retire.md), so effort there is wasted.
- Watch [Kotlin/JVM exit](/quest/m2/kt-jvm-exit.md), which depends on how the
  runtime thread stops.
- Test: the interop go and python publishers on a pinned single CPU and on
  many, and every binding's test suite. Benchmark a binding publisher's
  throughput before and after.

Public API: callbacks may fire concurrently on any runtime thread
(documented); no signature changes. Wire: none.

## Related

- [Serve budget](/quest/m0/serve-budget.md) - the root fix for a task hogging its thread
- [Kotlin/JVM exit](/quest/m2/kt-jvm-exit.md) - runtime-thread shutdown
- [Generated C bindings](/quest/m1/c/README.md) - inherits the moq-ffi runtime, so hand-written moq-c needs no change
