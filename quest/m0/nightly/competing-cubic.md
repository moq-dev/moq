# [S] Probe against Cubic passes

## Goal

`tests::probe::competing_cubic` in `rs/moq-quic/src/tests/probe.rs`, run by
Nightly through `just rs quic-sim`, passes on `main`, fixed at its cause.

## Plan

[moq-dev/moq#6719](https://github.com/moq-dev/moq/pull/6719) found it failing
the same way on `main` and on its branch: a probe resends a stream after that
stream was reset. A reset stream's data is never retransmitted, so find which
path still queues it (probe padding, a loss-recovery retransmit, or a stale
send queue entry) and stop it there. The other probe tests in the release
suite pass, so the fix must keep them passing.

Public API: none. Wire: none.
