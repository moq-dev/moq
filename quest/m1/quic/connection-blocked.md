# [S] Connection-blocked writers end cleanly

## Goal

In `moq-quic`, a send stream waiting on connection credit leaves the
blocked-writer list when it ends, and a write to a stopped stream returns
`Stopped` even while connection credit is exhausted. The fix is made once in the
shared stream engine, so tokio, io_uring, and qmux all get it.

Today a peer that grants streams but never MAX_DATA makes the relay keep an
entry for every stream it opens and cancels, with no bound. A writer parked on
`Blocked` also sleeps through a STOP_SENDING that has already arrived, until
unrelated credit shows up or the connection closes.

## Plan

Decided in the 2026-10-22 audit plan: one quest for both, because both bugs
sit on the same connection-blocked path in
`rs/moq-quic/src/connection/streams/`.

- Leak: `Streams::connection_blocked` is a `Vec` that `poll` drains only
  once `write_limit() > 0`, and `stream_freed` leaves it alone. Tie
  membership to stream lifetime by making it an insertion-ordered set (such as
  an `IndexSet`) and removing the entry when a send stream is freed or reset.
  Decided over deleting the list and scanning live streams when credit
  returns: a removal stays O(1), and the set never holds more entries than
  there are live streams.
- Hidden stop: `SendStream::write_source` returns `Blocked` on
  `limit == 0` before `Send::write` can look at `stop_reason`. Check for
  terminal stream state before connection-level flow control.
- Regression tests go in the existing `state.rs` unit tests. One test
  writes to many streams under zero connection credit, each write returning
  `Blocked`, then resets and frees those streams and asserts that the blocked
  set is empty. The other sends STOP_SENDING on one stream while
  another holds the connection window, and asserts that the write returns
  `Stopped`.

Declined from the same audit: qmux's `closed()` resolving once the FIN is
serialized, before the batch reaches the transport. qmux has no ACKs, so `Ok`
only ever meant "handed off locally". A FIN already in the kernel's
TCP/WebSocket buffer is lost the same way if the transport dies, and record
order still keeps finish, closed, close correct.
