# [XS] moq-uring's handle lifecycle tests cover the newly spawned queue and a woken close watcher

## Goal

Two regressions cover the shutdown paths
[moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309) added: a
`moq-uring` worker dropped while a connection driver is still in its newly
spawned queue (never polled) fails that connection's handles promptly, and
a reset arriving while moq-tokio's adapter `RecvStream` sits at its
read-ahead cap wakes the parked close watcher through the backend's own
wake, not a manual repoll. moq-tokio's test-only adapter read path is
narrowed if that removes adapter state.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309)
(its online review's non-blocking coverage suggestions), which finishes
[io_uring handle lifecycle](/quest/m2/uring-lifecycle.md).

- Newly spawned queue: [moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309) makes `Worker::drop` also drop tasks spawned
  since its last turn (`rs/moq-uring/src/worker.rs`, `spawn` at :609 and
  :692 on main) and gives `Driver` a `Drop` that fails the connection.
  Spawn a driver, drop the worker before its next turn, and check a handle
  fails with the worker-gone error instead of pending.
- Reset at the cap: [moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309)'s `recv_closed_sees_a_reset_at_the_cap` and
  `recv_closed_resumes_when_a_read_drains_the_cap` in
  `rs/moq-tokio/src/transport.rs` repoll `poll_closed` by hand. Use a
  backend (moq-uring or the real moq-tokio QUIC stream) whose reset fires
  the registered waker, and assert the watcher wakes on its own.
- The adapter read path [moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309) added is reached only by tests today; if a
  narrower shape passes both tests, take it.

Public API: none. Wire: none.

## Required

- [io_uring handles follow their worker and owner](/quest/m2/uring-lifecycle.md) - its PR, [moq-dev/moq#7309](https://github.com/moq-dev/moq/pull/7309), adds the paths these test
