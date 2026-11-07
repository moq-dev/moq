# [S] Validate PCR jumps across recovery

## Goal

Validate actual-PCR-jump flags in the remaining TS recovery interop coverage.

## Plan

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ Continue the actual-PCR-jump approach. Not chosen: rethink; drop the PR.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ Reproduce and fix the Rust `with_delay` schedule replacement in this PR. Hand the unrelated JS decoded-end/session-failure finding to the datagram-end close quest, fixed by [moq-dev/moq#8157](https://github.com/moq-dev/moq/pull/8157). Not chosen: fixing both findings here.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ Construction-only delay supersedes runtime updates. TS Export and Release take delay when constructed, so retained state never changes its delay basis. Not chosen: full runtime retiming; landing without the fix.
