# [S] macOS zero-RTT driver test passes in the merge queue

## Goal

`moq-tokio quic::driver::tests::zero_rtt` passes on the macOS Platform job, so
Platform can become a required merge-queue gate.

## Plan

- It fails on macOS merge-queue runs since #5664 moved moq-tokio onto
  `moq-quic` (runs 38924430657 and 38931667318). The failure is in the
  initial full-handshake connection, before any 0-RTT attempt: the server's
  0.5-RTT `open_uni` (`rs/moq-tokio/src/quic/driver/tests.rs:710`) and the
  client's first `accept_uni` (`:733`) both end with `TimedOut`. Linux
  passes.
- The test is a verbatim copy of quinn's; find what differs on macOS in the
  adapted driver (socket options, GSO/ECN, stream credit during the handshake) and fix the cause.
  Do not ignore the test on macOS.
