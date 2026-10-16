# [M] Spike: poll-native async in moq-ffi

## Goal

A report measures two kio-based async shapes against today's `ffi::Task`
on one hot path, and the maintainer picks which one moq-ffi migrates to.
This quest does not migrate moq-ffi; the migration is a follow-up quest.

## Plan

Decided with the maintainer on 2026-10-13:

- **Why**: moq-net is poll-based through kio, but moq-ffi does not use kio.
  `rs/moq-ffi/src/ffi.rs` wraps every handle in a tokio `Mutex`, a `watch`
  cancel flag, and `select!`. It spawns detached work with an
  `AbortOnDrop`, and needs a poll counter so shutdown can wait out
  in-flight calls. There are about 43 `run`/`detached` call sites, and its
  cancel bugs (#2932, the swallowed-event fix) came from that machinery.
- **Shape A, poll-native exports**: an async export is a `kio::wait` over the
  underlying `poll_*`, and UniFFI's `RustFuture` polls it directly. No
  per-handle mutex and no spawned task. UniFFI's cancel only stops polling
  and keeps the future until `rust_future_free`
  (mozilla/uniffi-rs#2771), and `ffi::Task::run` depends on that today. So
  Shape A includes the core change that drops the future on cancel,
  prototyped on a UniFFI branch if [the fork](/quest/m2/uniffi-fork.md)
  does not exist yet, and measures with and without it. Measure what
  remains of `Task` (cancel, shutdown).
- **Shape B, foreign poll**: export a sync `poll_x(waker)` whose waker is a
  foreign callback interface, and let each wrapper build its own future
  around it (asyncio, a Go channel, a coroutine). This works with UniFFI as
  it is. A generator-level poll protocol in the fork is a candidate too if
  the numbers justify it.
- **Hot path**: track `next` and group `read_frame`, prototyped in both
  shapes beside the existing `Task` version.
- **Measure** in Python and Go, on the [binding benchmark](/bench/README.md):
  per-call latency, wakeups per frame, throughput, and CPU. Sweep subscriber
  count so a cost that grows with the fan-out shows up. Test
  cancellation semantics too: a cancelled read must not swallow the next
  event, and shutdown mid-call must not crash at exit.
- No invented thresholds; the maintainer weighs the report. The migration
  quest records the chosen shape.

Public API: none. Wire: none.

## Related

- [Runtime trait](/quest/m2/runtime-trait.md) - replaces web-async; moq-ffi's tokio runtime may move behind it
