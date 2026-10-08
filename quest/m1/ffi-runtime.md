# [S] FFI apps run on more than one thread

## Goal

moq-ffi and moq-c drive moq on a multi-thread tokio runtime instead of one
`current_thread` runtime thread, so noq's endpoint and connection drivers
run beside a busy session, and Go, Python, Swift, Kotlin, and Dart apps get
the parallelism their own runtimes already have.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview (paper trail in the PR that
added this quest):

- Today `runtime()` in `rs/moq-ffi/src/ffi.rs` builds
  `new_current_thread().enable_all()` on one dedicated `moq-ffi` thread, and
  `rs/moq-c/src/ffi.rs` does the same (`moq-c`). Everything shares it: noq's
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
  that enter the runtime context (`MoqAudioProducer::write`), and keep
  `moq_ffi_shutdown` and Python's `atexit` clean.
- Watch [Kotlin/JVM exit](/quest/m2/kt-jvm-exit.md), which depends on how the
  runtime thread stops.
- Test: the interop go and python publishers on a pinned single CPU and on
  many, and every binding's test suite. Benchmark a binding publisher's
  throughput before and after.

Public API: none (threading only). Wire: none.

## Related

- [Serve budget](/quest/m0/serve-budget.md) - the root fix for a task hogging its thread
- [Kotlin/JVM exit](/quest/m2/kt-jvm-exit.md) - runtime-thread shutdown
