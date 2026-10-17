# [L] Poll-native async in moq-ffi

## Goal

Every moq-ffi async export over kio-backed state (track, group, and datagram
reads, announcements, the JSON and media consumers) is a `kio::wait`
over the underlying `poll_*`, which UniFFI's `RustFuture` polls directly.
`ffi::Task`'s tokio `Mutex`, `watch` cancel flag, `select!`, and in-flight
poll counter are gone from those paths. Export signatures, and so every
binding, are unchanged.

## Plan

Shape A from [the spike](https://github.com/moq-dev/moq/pull/5851), pending
the maintainer's pick there. The prototype is on the
`spike/ffi-poll-prototype` branch.

- **What the spike found**: in Go, Shape A cut a ready `read_frame` from 2.3
  to 1.2 µs, and where every read parks it raised throughput 13 to 32 percent
  and cut CPU per frame 20 to 30 percent across 1 to 64 subscribers. Python
  was within noise, since asyncio's own wakeup dominates. With
  no read lane, a cancelled read that UniFFI has not freed yet no longer
  blocks the next read, so this needs no UniFFI change. kio-only reads also
  keep working after `moq_ffi_shutdown`.
- **Handle state**: a std `Mutex` held only across one poll, never across a
  wait. Cancel closes a kio channel that every poll checks, then drops the
  state. Driver supervision (`Background`) is kio already, so poll it in the
  same closure.
- **Concurrency**: two concurrent reads on one handle no longer queue FIFO;
  both park, and each poll takes the next item atomically. Check wrapper docs
  that promise serialization.
- **Still lost on cancel**: when a RustFuture finishes and the foreign side
  then drops it as cancelled (Python asyncio cancel, Go ctx), the result is
  gone. The spike measured this in both `Task` and Shape A. Keep the Go
  wrapper's terminal cancel. Only a foreign poll protocol fixes it.
- **What stays**: exports that need tokio I/O (session connect, server
  accept, demand) stay `detached`. Keep shutdown's in-flight counter only for
  those.
- **Tests**: a read cancelled but not yet freed does not block the next read
  on the same handle (drive `rust_future_*` directly, as the spike did), and a
  read parked across shutdown resolves without a crash.
- Watch the binding benchmark; the Go runner should move.

Public API: none, apart from per-handle FIFO queueing. Wire: none.

## Related

- [Hard-fork the binding generator](/quest/m2/uniffi-fork.md) - where a generated foreign poll protocol (the spike's Shape B′) would live
- [Runtime trait](/quest/m2/runtime-trait.md) - moq-ffi's remaining tokio use may move behind it
