# [M] io_uring handles follow their worker and owner

## Goal

Every `moq-uring` QUIC handle fails promptly once its worker or connection
is gone, keeps data it already received, and closes its connection when the
last handle drops, matching the tokio backend's contract.

## Plan

Findings from the 2026-10-22 io_uring audit, none reachable by a peer
against the relay (its handles live and die inside worker tasks), all
reachable by an embedder:

- Worker gone: `Inner::closed` (`rs/moq-uring/src/quic/driver/connection.rs`)
  reads only the cached state, `Driver` has no `Drop`, and `Worker::drop`
  drops tasks without failing them. A `Connection` or stream returned out of
  `block_on` then pends forever on `closed` or accept while writes and
  datagrams still succeed. The worker doc promises handles that outlive it
  fail instead of pending; sockets and endpoints already do. Propagate
  termination to every surviving handle and wake pending operations,
  keeping received data readable.
- Drop closes: decided 2026-10-22 (maintainer), match tokio. A raw
  `Connection` closes once every connection and stream handle is gone,
  instead of leaking until idle timeout, and `web::Session` counts live
  streams rather than closing on its last clone. Update the `Connection`
  doc that states the opposite.
- Receive close at the read-ahead cap: moq-tokio's adapter
  (`RecvStream::poll_closed` in `rs/moq-tokio/src/transport.rs`) returns
  Pending without registering a waker once its read-ahead reaches
  `READ_AHEAD_CAP` (64 KiB), so a reset or connection close (idle timeout
  included) behind that much unread data never surfaces. Check terminal
  state before the cap and register the closed watcher so a read that
  drains below the cap wakes it. Test with `poll_closed` and the draining
  read in separate tasks. moq-uring's own `READ_AHEAD` backlog is gone:
  [moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878) replaced it
  with receive leases, whose `poll_lease` checks `closed()` when blocked.

Use the existing teardown and credit integration tests' shapes; drive time
with the manual clock for the idle-timeout case.

Public API: behavior only (implicit close on drop). Wire: none.

## Related

- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - requires this
