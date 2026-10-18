# [S] The uring teardown close test holds under load

## Goal

`moq-uring`'s `a_published_close_has_already_left_the_client`
(`rs/moq-uring/tests/integration/teardown.rs`) passes under a loaded `just check`,
fixed at its cause. It still proves that a close the client reported as
published reaches the server as the application code, not an idle timeout.

## Plan

Seen 2026-10-13 on #5483's `just check` at a load average near 48: the test
hung until nextest's 60 s timeout. It passed alone and 30 of 30 times under
`--stress-count`. The test uses real UDP sockets and two `Worker` threads,
and the server waits on `poll_closed` with no deadline of its own, so a
CONNECTION_CLOSE that never leaves the client, or a server wake that is
lost, both look like this hang.

Decided 2026-10-13 (maintainer): a child of this questline, m1. Reproduce it
under load first (for example the test in a loop beside a CPU hog, or
`--stress-count` with `just check` running). Then tell the two causes apart:

- The client stops its runtime before the close datagram is on the wire.
  That is a product bug: "published" must mean sent. Fix the teardown.
- The server's `poll_closed` misses the wake for a close it received. Fix
  the wake.

Never widen a timeout or add a retry. Land a regression that fails without
the fix where one is easy.

Public API: none expected. Wire: none.
