# [S] Diagnose and re-enable the FFI datagram connection test

## Goal

`datagrams_cross_a_default_connection` and its `requested_` sibling
(`rs/moq-ffi/src/test.rs`) pass reliably under CI load after the timeout
cause is confirmed and fixed.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7914](https://github.com/moq-dev/moq/pull/7914)): ✅ m0.

Decided 2026-11-05 (maintainer, /quest-plan): ✅ Keep only
`datagrams_cross_a_default_connection` quarantined and land the quarantine in
[moq-dev/moq#7954](https://github.com/moq-dev/moq/pull/7954). Re-diagnose under
CI load after [moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951)
lands. Re-enabling now is rejected. Do not add repeated sending without
evidence of ordinary loss.

The test sends one datagram over a real connection and waits 10 s for it;
it timed out in Check on
[moq-dev/moq#7914](https://github.com/moq-dev/moq/pull/7914) while passing
30 of 30 runs on an idle host. Confirm the cause with send/route diagnostics
before choosing the fix and re-enabling the test.

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
  the repository's flaky-test rule. No retry or longer wait was added.
- `requested_datagrams_cross_a_default_connection` is quarantined too, by
  [moq-dev/moq#7904](https://github.com/moq-dev/moq/pull/7904): its single
  datagram missed the 10 s wait in that PR's
  [Check](https://github.com/moq-dev/moq/actions/runs/42076855917/job/127419261608)
  (panic at `rs/moq-ffi/src/test.rs:6111`), the same symptom as the plain
  test, then passed 100 of 100 stress runs on an idle host. Re-enable it with
  the plain test once this quest's cause is fixed; its other requested
  siblings stay enabled.

## Waiting

- moq-dev/moq: capture a failing run under CI load with send/route
  diagnostics ([moq-dev/moq#7951](https://github.com/moq-dev/moq/pull/7951)
  merged 2026-11-06). Run the ignored case locally under CI-like concurrent
  build/test load, with diagnostics at `DatagramSend::send` and
  `Subscriber::route_datagram`, retaining a failing trace. Explicitly include
  the quarantine with `quest gate just rs test -p moq-ffi --lib
  -E 'test(datagrams_cross_a_default_connection)' --run-ignored all --no-capture`;
  normal Check and queue runs intentionally skip it. Use it to distinguish ordinary transport loss, a route miss, or
  another lifecycle bug and choose the corresponding fix before re-enabling.
  Passing stress runs alone do not clear this wait or establish transport loss.
  No workflow dispatch or host change is authorized for this investigation.
