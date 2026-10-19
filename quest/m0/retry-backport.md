# [S] Backport the discarded-Retry fix to the release line

## Goal

A client built from `release` that discards an invalid Retry (a bad integrity
tag, or a source CID equal to its original destination CID) still follows the
server's genuine Retry and connects, and still honors a Version Negotiation
packet. Today one injected or corrupted Retry kills its handshake against any
server that validates addresses.

## Plan

`release` pins moq-noq-proto 1.3.5, which has the same `total_authed_packets`
guard that `main` replaced in `moq-quic`. Two PRs, per CONTRIBUTING.md:

1. moq-dev/noq: port the fix and its regression tests onto `release/1.3`, then
   publish moq-noq 1.3.6.
2. This repo: bump moq-noq onto `release` so it ships in the next 0.17 patch.

Public API: none. Wire: none.
