# [S] A JS live reader keeps a history reader's floor

## Goal

When a JS track has a live reader with no `ranges` and a history reader at the
same time, the subscription's demand still reaches back to the history
reader's floor. Settle with a test whether it does. Fix it if it doesn't, or
delete this quest if the test shows it already does.

## Plan

Decided 2026-11-13 (maintainer, /quest-run follow-up from
[moq-dev/moq#8643](https://github.com/moq-dev/moq/pull/8643)): ✅ m1 (rejected: m0, m2).

Unverified. A scratch test during #8643 gave a demand of `[{}]` for that pair,
which looks like an open live range with no floor. Start from a test that
pins the demand the pair should produce, and compare with Rust's equivalent.
[moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616) (lite-07 FETCH
single-group only) rewrote how JS keeps fill and demand ranges apart
(`LiveRanges.edit`), so check against that code.

Public API: none expected. Wire: none.
