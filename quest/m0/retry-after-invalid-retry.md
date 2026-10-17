# [S] A discarded Retry no longer blocks the genuine one

## Goal

A client that discards an invalid Retry (a bad integrity tag, or a source CID
equal to its original destination CID) still follows the server's genuine
Retry and connects. Today the discarded Retry bumps `total_authed_packets`,
so the client then discards the genuine Retry too and never connects: one
injected or corrupted Retry kills a handshake against any server that
validates addresses.

`retry_after_invalid_retry` in
`rs/moq-quic/src/tests/characterize/paths.rs` must pass with its `#[ignore]`
removed, while `invalid_retry_is_discarded` and `second_retry_is_discarded`
keep passing.

## Plan

Decided 2026-10-16 (maintainer): m0, because the shipped release has the same
Retry guard in moq-noq-proto 1.3.5.

The Retry arm of `process_decrypted_packet` in
`rs/moq-quic/src/connection/mod.rs` uses `total_authed_packets > 1` to mean
"already processed a server Initial or Retry", but every Retry reaches
`on_packet_authenticated` before its validity checks run. Make the guard
mean what RFC 9000 17.2.5.2 says: count only a Retry the client accepts, or
track that state explicitly. The VersionNegotiate arm shares the counter;
check whether a discarded Retry also makes it ignore a genuine Version
Negotiation.

Land the fix on `main` first. The `release` backport follows CONTRIBUTING.md
as separate PRs: a moq-dev/noq 1.3.x patch to moq-noq-proto, then a moq-noq
bump onto `release`. The PR that lands the main fix adds that backport quest.
