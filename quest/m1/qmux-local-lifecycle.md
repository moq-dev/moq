# [S] qmux aborts and closes without the peer's help

## Goal

In the in-tree `@moq/qmux` (`js/qmux`), `writer.abort()` on a stream whose
write is waiting for stream or connection send credit rejects that write and
sends RESET_STREAM at once. `close()` closes the socket only after
APPLICATION_CLOSE is handed to it, or after a bounded stall: a backed-up
WebSocket that drains in time delivers the frame, and one still wedged at the
deadline closes without it. Neither outcome waits for the peer to grant
credit, and close never relies on a fixed delay. Rust
`rs/qmux` already behaves this way and is out of scope.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): one quest for both,
since both live in `js/qmux/src/session.ts` and share one contract: local
lifecycle never depends on peer cooperation. Both were deferred by
[moq-dev/moq#6124](https://github.com/moq-dev/moq/pull/6124).

Abort: the sink's `write` reaches `#claimSendCredit`, which awaits
`flow.sendCredit.claim()` and `#connCredit.claim(1n)`. `Credit.claim` in
`js/qmux/src/credit.ts` takes no signal, and WritableStream runs the sink's
`abort` only after the in-flight write settles, so the abort waits for credit.
Give `Credit.claim` an `AbortSignal` that removes its waiter and rejects, and
pass the sink controller's `signal` down through the stream data path, so the
existing abort handler sends RESET_STREAM immediately.

Close: `close()` and `#sendConnectionClose` queue APPLICATION_CLOSE, then
`sleep(100ms)` before `#transportClose()`. The scheduler holds the frame until
`sink.ready()`, and the WebSocket ponyfill holds a write while
`bufferedAmount` is high, so a backed-up socket closes with the frame never
sent. Replace both sleeps with a promise the scheduler resolves once the
control lane has flushed. Drop queued stream frames first, so
APPLICATION_CLOSE goes out next. Bound the wait like Rust's `CLOSE_TIMEOUT`
(`rs/qmux/src/session.rs`), started only while the writer is stalled.

Test with the controlled clock and a wedged socket: an abort blocked on each
credit kind emits RESET_STREAM with no peer grant; a close behind a socket
that drains before the deadline writes APPLICATION_CLOSE before the transport
closes; and one that never drains closes at the deadline without it.

Public API: none. Wire: none.
