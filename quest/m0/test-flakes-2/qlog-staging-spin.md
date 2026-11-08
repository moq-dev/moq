# [XS] moq-uring's qlog accounting test waits on the accounting

## Goal

moq-uring's `quic::qlog::tests::an_open_trace_accounts_for_staging_and_close`
passes on a loaded host. It waits for the staged memory to be accounted, not
for a fixed number of spins.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8208](https://github.com/moq-dev/moq/pull/8208).

The test (`rs/moq-uring/src/quic/qlog.rs`) spins up to 10,000
`std::thread::yield_now` calls until `sink.inner.memory` reaches the
expected value, which a background thread sets. On a loaded host the thread
can run late and the spins run out first. Wait on the accounting itself, for
example a condition the sink signals when it charges memory, or a blocking
read of the staged state. Don't raise the spin count.

Public API: none. Wire: none.
