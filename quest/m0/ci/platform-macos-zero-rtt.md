# [S] macOS zero-RTT driver test passes in the merge queue

## Goal

`moq-tokio quic::driver::tests::zero_rtt` passes on the macOS Platform job, so
Platform can become a required merge-queue gate.

## Plan

- It fails on macOS merge-queue runs since #5664 moved moq-tokio onto
  `moq-quic` (runs 38924430657 and 38931667318). The connect future at
  `rs/moq-tokio/src/quic/driver/tests.rs:733` fails (`expect("connect")`) on
  the 0-RTT attempt. Linux passes.
- The test is a verbatim copy of quinn's; find what differs on macOS in the
  adapted driver (socket options, GSO/ECN, ticket handling) and fix the cause.
  Do not ignore the test on macOS.
