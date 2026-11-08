# [XS] moq-uring's qlog accounting test waits on the accounting

## Goal

moq-uring's `quic::qlog::tests::an_open_trace_accounts_for_staging_and_close`
passes on a loaded host. It waits for the staged memory to be accounted, not
for a fixed number of spins.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8208](https://github.com/moq-dev/moq/pull/8208).

The test (`rs/moq-uring/src/quic/qlog.rs`) spins up to 10,000
`std::thread::yield_now` calls until `sink.inner.memory` drops to
`CHUNK + size_of::<Msg>()`. `Inner::reserve` charges synchronously; the drop
comes from the writer thread releasing the `Msg::Open` charge
(`memory.fetch_sub`) once it handles it. On a loaded host the writer can run
late and the spins run out first. Wait for that release itself, for example
a signal the writer raises after it handles a message. Don't raise the spin
count.

It has not failed in CI yet, so it stays enabled; quarantine it with
`#[ignore]` and a `sh/gh/test-inventory.allow` entry if it does.

Public API: none. Wire: none.
