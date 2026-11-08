# [XS] moq-tokio's stream-stopped tests survive a loaded host

## Goal

moq-tokio's `quic::driver::tests::stream_stopped` runs again and passes on a
loaded host. A stopped stream is proven by the stop event it awaits, not by
a wall-clock deadline.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088), which
quarantined the test with `#[ignore]` and a `sh/gh/test-inventory.allow`
entry naming this epic.

The test (`rs/moq-tokio/src/quic/driver/tests.rs`) wraps its client in
`timeout(Duration::from_millis(100), ...)`. Under load the dial, write, and
stop round trip takes longer, so it fails even though nothing is wrong.
nextest's 60 s `terminate-after` already bounds a real hang. Drop that
deadline, so the test awaits the stop codes it asserts.
`stream_stopped_2` in the same file bounds `closed()` with the same 100 ms
`timeout`; fix it the same way, unless the deadline is what it asserts. Then
remove the `#[ignore]` and the allowlist entry, and delete this quest.

Public API: none. Wire: none.
