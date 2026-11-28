# [S] A coalesced Restart and Unannounce for a lost identity reads as Offline

## Goal

When a path's serving identity is lost and its Restart and Unannounce coalesce before a reader
polls, the origin's announcement consumers (Rust `origin::Consumer`, `follow`, and the JS
`Routes` stream) report the path `Offline`, as they would uncoalesced, never `End`. A reader that
sees `End` treats the broadcast as finished when it only went offline.

## Plan

Noted during [moq-dev/moq#9800](https://github.com/moq-dev/moq/pull/9800) and not confirmed; it
predates that PR. Reproduce first with a test that queues a Restart and then an Unannounce for the
same lost identity before one read, in Rust and JS. If it reproduces, keep the `Offline` meaning
through the pending-event overwrite (`OriginConsumerState` in Rust; `follow.rs`'s `fold` maps
kinds). If it does not, delete this quest and say so in the PR. The JS side has no coalescing yet
([JS announced coalescing](/quest/m3/js-announced-coalesce.md)), so keep its test for when that
lands.

Decided 2026-11-28 (maintainer): ✅ plan it in m3 (rejected: drop until someone hits it).

Public API: none. Wire: none.

## Related

- [JS announced coalescing](/quest/m3/js-announced-coalesce.md) - brings the same overwrite to JS, where this must hold too
