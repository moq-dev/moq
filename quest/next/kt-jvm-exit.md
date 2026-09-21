# [S] Kotlin never crashes at JVM exit

## Goal

A Kotlin/JVM program using moq exits cleanly whatever the moq-ffi runtime
thread is doing at the time, proven by a regression test that forces the
racing order. The Python binding hit this class of crash (a runtime-thread
callback into a host that has begun finalizing) about one run in four under
load; the JVM has the same shape and nothing exercises it today. Android is
out of scope: it kills the process rather than destroying the VM.

## Plan

- Audit first. uniffi delivers async continuations as JNA callbacks
  (`UniffiRustFutureContinuationCallback` in
  `kt/moq-ffi/src/jvmAndAndroidMain/kotlin/uniffi/moq/moq.kt`, bound with
  `Native.register`) invoked on the `moq-ffi` runtime thread. Establish what
  a JNA callback does when it fires after `DestroyJavaVM` has begun, or after
  the last non-daemon thread exits: attach failure, `Callback after VM
  shutdown` abort, or a hang. Reproduce with a child JVM that delays a
  continuation on the runtime thread while `main` returns.
- Fix only what reproduces. The expected fix is a hand-written Kotlin file in
  `kt/moq-ffi` (not the generated `moq.kt`, and not only the `kt/moq`
  wrapper, since raw uniffi consumers exist) that binds `moq_ffi_shutdown`
  through JNA against the same library and registers
  `Runtime.addShutdownHook` on first load. Shutdown hooks run before VM
  destruction, so the thread finishes its current callback and never starts
  another. The hook must never run on the runtime thread itself, which would
  wait on itself.
- Regression test: a JUnit test in `kt/moq-ffi` that forks a JVM in the
  racing order (mirror `py/moq-ffi/tests/exit_inside_callback.py`) and
  asserts a zero exit code and no `panicked` on stderr, with and without
  `RUST_BACKTRACE=1` since symbolization can mask the abort behind a clean
  exit code. Run it in the existing `kt` CI job.

Public API: none; the hook is internal to the binding. Wire: none.

## Related

- [libmoq shutdown](/quest/next/libmoq-shutdown.md) - the same hazard class for the C ABI and the OBS plugin
