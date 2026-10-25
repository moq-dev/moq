# [S] macOS receive-credit timeout

## Goal

`receive_credit::partial_control_requests_give_way`
(`rs/moq-tokio/tests/integration/receive_credit.rs:502`) passes on Nightly's
macOS lane.

## Plan

It panics "stop timed out" on macOS (real sockets, 10 s timeout) while all
nine `receive_credit` tests pass on Linux. macOS failed a different test on
each of 10-22, 10-23, and 10-24 (10-23:
`quic::driver::tests::stuck_window_evicts_the_lower_priority`), so first
check for a shared platform cause, such as socket buffer sizes or timer
resolution, before fixing tests one by one.

Public API: none. Wire: none.
