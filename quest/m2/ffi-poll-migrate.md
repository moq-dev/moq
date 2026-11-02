# [L] Poll-native async in moq-ffi

## Goal

Every moq-ffi async export over kio-backed state (track, group, and datagram
reads, announcements, the JSON and media consumers) is a `kio::wait`
over the underlying `poll_*`, which UniFFI's `RustFuture` polls directly.
`ffi::Task`'s tokio `Mutex`, `watch` cancel flag, and `select!` are gone
from those paths. Export signatures, and so every
binding, are unchanged.

## Plan

Decided with the maintainer on 2026-10-16, after
[the spike](https://github.com/moq-dev/moq/pull/5851): Shape A. The
prototype is on the `spike/ffi-poll-prototype` branch; delete that branch
once this lands.

- **No UniFFI change on this path**: dropping a future at
  `rust_future_cancel` (mozilla/uniffi-rs#2771) is optional in
  [the fork](/quest/m2/uniffi-fork.md). Shape A has no read lane for an
  unfreed future to hold, so this quest must not wait for it.
- **What the spike found**: in Go, Shape A cut a ready `read_frame` from 2.3
  to 1.2 µs, and where every read parks it raised throughput 13 to 32 percent
  and cut CPU per frame 20 to 30 percent across 1 to 64 subscribers. Python
  was within noise, since asyncio's own wakeup dominates. With
  no read lane, a cancelled read that UniFFI has not freed yet no longer
  blocks the next read, so this needs no UniFFI change.
- **Keep shutdown's contract**: `moq_ffi_shutdown` documents that every
  parked and later call resolves `Cancelled`. The spike's prototype broke
  that: its kio reads delivered frames after shutdown. Each poll runs inside
  shutdown's in-flight accounting (today's `Polling` guard, or something as
  strong), so shutdown cannot return while a poll that missed it still
  delivers an item.
- **Handle state**: a std `Mutex` held only across one poll, never across a
  wait. Cancel closes a kio channel that every poll checks, then drops the
  state. Driver supervision (`Background`) is kio already, so poll it in the
  same closure.
- **Concurrency**: two concurrent reads on one handle no longer queue FIFO;
  both park, and each poll takes the next item atomically. Iteration order
  (sequence or arrival) is unchanged; only which concurrent call gets which
  item is. Check wrapper docs that promise serialization.
- **Multi-step reads**: an operation that spans polls through shared handle
  state must tolerate another call changing it between polls. The track's
  `read_frame` acquires a group into the shared `pending` slot, then reads
  from it on a later poll. A concurrent `next_group`, or a second
  `read_frame`, can take that group in between, and the current `expect` would
  panic. Either make the whole step one poll, re-acquire when the slot is
  empty, or keep per-operation ownership for these reads.
- **Still lost on cancel**: when a RustFuture finishes and the foreign side
  then drops it as cancelled (Python asyncio cancel, Go ctx), the result is
  gone. The spike measured this in both `Task` and Shape A. Keep the Go
  wrapper's terminal cancel. [Foreign poll](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-poll-protocol.md)
  fixes it later for the hot reads.
- **What stays**: migrate an export only when it is pure kio polling over
  handle state. Anything that awaits tokio channels, codecs, or I/O keeps
  `Task`, which supplies its serialized state, terminal cancel, and
  shutdown's in-flight counter. Examples: client `connect`, server `listen`
  and `accept`, request `accept` and `reject`, session `closed` and `status`,
  and the video and audio consumers' `next`, which drive codecs (cancelling a
  queued video decode poisons its sink). Demand waits and session `shutdown`
  stay `detached`. Inventory every `Task::run` and `detached` site in the PR, each marked
  migrated or kept, with the reason.
- **Tests**: a read cancelled but not yet freed does not block the next read
  on the same handle (drive `rust_future_*` directly, as the spike did). A
  read parked across shutdown resolves `Cancelled` without a crash. Two reads
  parked on one handle each wake, and between them they take every item
  exactly once. That includes a mixed `read_frame` and `next_group` on one
  track.
- **Benchmark both fan-out axes**: the spike swept subscribers at one
  publisher. Confirm the gain on the binding benchmark across publishers and
  subscribers before and after; the Go runner should move.

Public API: no signature changes. Concurrent reads on one handle no longer
get items in call order; sequence and arrival order are unchanged. Wire: none.

## Related

- [Foreign poll for hot reads](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-poll-protocol.md) - the generated protocol that follows this, once the fork exists
- [Libraries spawn nothing](/quest/m1/poll-first/no-spawn.md) - moq-ffi spawns only its root at the binding boundary
