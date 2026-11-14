# [XS] moq-uring's handle lifecycle tests cover a driver in the newly spawned queue

## Goal

A regression covers the shutdown path
[moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309) added: a
`moq-uring` worker dropped while a connection driver is still in its newly
spawned queue (never polled) fails that connection's handles promptly.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309)
(merged; its online review's non-blocking coverage suggestions), which
finished the io_uring handle lifecycle quest.

[moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309) makes `Worker::drop` also drop tasks spawned
since its last turn (`rs/moq-uring/src/worker.rs`, the `spawns` queue taken in
`impl Drop for Worker`) and gives `Driver` a `Drop` that fails the connection
(`impl Drop for Driver` in `rs/moq-uring/src/quic/driver/connection.rs`).
Only `dropped_worker_rejects_operations` covers a dropped worker today. Spawn
a driver, drop the worker before its next turn, and check a handle fails with
the worker-gone error instead of pending.

The other half of the original quest, a reset at moq-tokio's adapter
read-ahead cap waking the close watcher, is gone:
[moq-dev/moq#7771](https://github.com/moq-dev/moq/pull/7771) replaced that
adapter read path, and its `recv_closed_*_cap` tests with it.

Public API: none. Wire: none.
