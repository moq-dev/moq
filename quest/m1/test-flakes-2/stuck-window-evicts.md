# [S] Stuck-window eviction test on macOS

## Goal

`quic::driver::tests::stuck_window_evicts_the_lower_priority`
(`rs/moq-tokio/src/quic/driver/tests.rs`) passes reliably on macOS, fixed at
its cause.

## Plan

It failed in Platform run
[40232491446](https://github.com/moq-dev/moq/actions/runs/40232491446) (job
121287165236): the evicted low-priority writer's `stopped()` returned `None`
instead of `Some(0)`, then passed in the next two runs. The likely cause is a
real-socket race: the low stream's write and FIN complete before
STOP_SENDING arrives. Reproduce it first, then make the ordering
deterministic per this epic's rules (hold the low stream open until the
eviction is observed, or drive it without real-socket timing), never by
loosening the assertion.

The deleted macOS `receive_credit` quest mentioned this test, but
[moq-dev/moq#6732](https://github.com/moq-dev/moq/pull/6732) fixed only
`receive_credit`.

Public API: none. Wire: none.
