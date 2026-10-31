# [S] PRs build the feature shapes they can break

## Goal

A PR that narrows which features compile a function's callers fails the merge gate when it leaves that function unused in some isolated feature build, instead of breaking Nightly's `rs features` job after it lands.

## Plan

The source-based selection is implemented in [moq-dev/moq#7433](https://github.com/moq-dev/moq/pull/7433). Remaining before completion: pass the local Rust gate, measure the added warm-cache wall time, and finish the local other-vendor review. Keep the quest until that validation is complete. The newly selected Tokio WebRTC shape needs the existing fix in [moq-dev/moq#7380](https://github.com/moq-dev/moq/pull/7380).

Found by the agent on [moq-dev/moq#7380](https://github.com/moq-dev/moq/pull/7380): only Nightly runs every isolated `moq-tokio` feature build (`just rs tokio-features --all`), plus PRs that change feature definitions. [moq-dev/moq#7120](https://github.com/moq-dev/moq/pull/7120) fixed one unused helper and missed `Addr::with_url`, because nothing on the PR built `--features webrtc` alone.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Select the isolated builds for a PR from the crates its diff touches, not only from changes to feature tables: a diff touching `rs/moq-tokio/src` runs moq-tokio's isolated shapes. Keep the gate's time budget from [the five-minute quest](/quest/m0/ci/five-minutes.md): if every shape is too slow, run them through `cargo check` (not tests), and only for crates whose source changed. Measure the added wall time on the queue runner and record it in the PR.

Public API: none. Wire: none.

## Related

- [Gate each diff in under five minutes](/quest/m0/ci/five-minutes.md) - the time budget this must fit in
- [Unify dependency features](/quest/m0/ci/hakari.md) - shares the feature-shape plumbing
