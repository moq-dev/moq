# [S] moq-ffi's datagram tests don't hinge on one datagram

## Goal

`datagrams_cross_a_default_connection` and its `requested_` sibling
(`rs/moq-ffi/src/test.rs`) pass reliably under CI load, without depending on
a single unreliable datagram arriving.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7914](https://github.com/moq-dev/moq/pull/7914)): ✅ m0.

The test sends one datagram over a real connection and waits 10 s for it;
it timed out in Check on #7914 while passing 30 of 30 runs on an idle host.
Confirm the cause first. If it's loss, keep writing datagrams until the first
arrives and assert on what arrives, rather than waiting longer. If it's the
datagram answer race planned in
[moq-dev/moq#7929](https://github.com/moq-dev/moq/pull/7929)
(`quest/m1/datagram-tracks/answer-race.md`), say so here and wait on that
quest instead.
