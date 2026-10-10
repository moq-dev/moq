# Release backports, second batch

## Goal

The 0.17.x line picks up the remaining fixes from the 2026-10-09 backport
triage that were left out of the first batch, each as its own PR onto
`release` (CONTRIBUTING), or the quest is deleted once a fix proves unneeded.

## Plan

The first batch (#5120 through #5135, plus #5164) covered every crash, leak,
and interop break the triage found. These children are smaller release-only
patches; most can't cherry-pick because `main` built them on work `release`
lacks (#4741, #4268, #4658's `request_stream.rs`). Write the release patch
against release's own code, carry the original regression test, and confirm
it fails on `release` first. Interop at Seattle (2026-10-12) may raise or
drop each one's priority.

The `release` interop matrix only negotiates moq-lite, so IETF changes need
unit tests or `just test bare-fin`-style coverage rather than `just test
interop`.

## Required

- [FETCH survives a request FIN](/quest/m1/release-backports/fetch-fin.md) - a d19+ FETCH keeps serving after the requester FINs, as SUBSCRIBE does since #5133
- [Repeated unknown SETUP options](/quest/m1/release-backports/setup-repeat.md) - a peer repeating an unknown or GREASE SETUP option no longer closes the session
- [Unknown request parameters](/quest/m1/release-backports/request-params.md) - draft 14-17 peers sending FORWARD or an unlisted parameter no longer close the session
- [NAMESPACE fill on d16+](/quest/m1/release-backports/namespace-fill.md) - a d16+ SUBSCRIBE_NAMESPACE without SOLICIT still gets NAMESPACE entries
- [JS keeps requests alive after FIN](/quest/m1/release-backports/js-fin.md) - the release-line `@moq/net` publisher stops cancelling on a d19+ request FIN
- [Quiet catalog after a cleared floor](/quest/m1/release-backports/catalog-floor.md) - a relay serves a quiet catalog after a resume floor clears, if the stall reproduces on `release`
