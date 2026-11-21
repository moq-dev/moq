# [S] moq-transport JS subscribers get the catalog's joining FETCH

## Goal

The scheduled Interop's `js -> js`, `js -> js-native-node` and
`js -> js-native-bun` cells pass on moq-transport again: the relay answers
the joining FETCH for a JS-published track instead of refusing it.

## Plan

Found by the 2026-11-20 triage of scheduled Interop runs; failing every night
since 2026-11-17, still on 11-20
([44776434797](https://github.com/moq-dev/moq/actions/runs/44776434797)).
The catalog SUBSCRIBE succeeds, but the joining FETCH is refused with
`joining prefix unavailable` (`rs/moq-net/src/ietf/publisher.rs`, the
`Error::NotFound` reject), and the catalog track never gets another group,
so the subscriber times out (`timed out waiting for data`).

Decided 2026-11-20 (maintainer, /quest-plan): ✅ m0 under interop-green
(rejected: m3), since this cell keeps scheduled Interop red every night and
hides new breaks.

- Reproduce with
  `just test interop --publishers js --subscribers js-native-node --versions moq-transport-18`.
- Prime suspect: [moq-dev/moq#8918](https://github.com/moq-dev/moq/pull/8918)
  (group IDs carried verbatim on moq-transport, merged 2026-11-17). Bisect
  13e593cdb1..bc66d3eaec if reverting it doesn't clear the cell.
- Related, not the same: [the kept draft-18 join](/quest/m3/ietf-join-pin-head.md)
  covers a Rust publisher dropping its producer after `finish()`.

Caused by: [moq-dev/moq#8918](https://github.com/moq-dev/moq/pull/8918), if
the bisect confirms it.

Public API: none expected. Wire: none.
