# [S] Validate PCR jumps across recovery

## Goal

Validate actual-PCR-jump flags in the remaining TS recovery interop coverage.

## Plan

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ Continue the actual-PCR-jump approach. Not chosen: rethink; drop the PR.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ Reproduce and fix the Rust `with_delay` schedule replacement in this PR. Hand the unrelated JS decoded-end/session-failure finding to [datagram-end-close](/quest/m0/test-flakes-2/datagram-end-close.md). Not chosen: fixing both findings here.
