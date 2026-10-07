# [S] moq-tokio's stream adapter wakes a close-only waiter

## Goal

`poll_closed` in moq-tokio's stream adapter (used by WebSocket, TCP, and Unix
sockets) always registers the caller's waker before returning `Pending`, so a
task that only waits for the stream to close wakes when the peer sends a FIN,
even after the read-ahead cap is hit.

## Plan

`rs/moq-tokio/src/transport.rs` (around line 770) returns `Pending` without
registering `cx` once read-ahead is full. The comment there relies on the
caller's own reads re-polling, which is not part of the trait's contract. The
io_uring receive stream parks on its readable list instead. Nothing in the
repo is a close-only caller today, so the bug is in the contract, not in a
user-visible path. Register the waker, or park it the way io_uring does, and
add a test with mocked time: fill the read-ahead, FIN, and assert that a
`poll_closed`-only waiter wakes.

Public API: none. Wire: none.
