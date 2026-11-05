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

Investigation 2026-11-05 on `main` at `419b799c75`:

- The unchanged three connection cases pass locally. With temporary diagnostic
  prints at `DatagramSend::send` and `Subscriber::route_datagram`,
  `quest gate just rs test -p moq-ffi --lib -E 'test(datagrams_cross)'
  --stress-count 100 --no-capture` passes all 300 cases. Every send returns
  `Ok(())`, and all 300 received datagrams find a route. The diagnostic
  prints were removed. This does not establish the cause of the CI timeout.
- All three tests await `consumer.info()` before appending. In
  `DatagramServe::poll_serve`, the answer decodes before `request.accept`
  exposes that info; route registration follows synchronously on that driver
  before its next datagram receive pass. The answer race in
  [moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951) is real,
  but was not reproduced by these tests. Do not claim it caused this timeout.
- `datagrams_cross_a_default_connection` is disabled pending diagnosis, per
  the repository's flaky-test rule. Its requested siblings have not failed
  in this investigation and remain enabled. No retry or longer wait was added.

## Waiting

- moq-dev/moq: capture a failing run with send and route diagnostics to
  distinguish transport loss from an unroutable datagram or another lifecycle
  bug. Recommendation: rerun the ignored case under the CI workload after
  [the answer-race fix](https://github.com/moq-dev/moq/pull/7951) lands,
  retaining diagnostics if it fails. A passing stress run is not proof of
  transport loss. Once the cause is confirmed, apply the corresponding Plan
  above and re-enable the test. No workflow dispatch or host change is
  authorized for this investigation.
