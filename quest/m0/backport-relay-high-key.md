# [M] Backport the far-ahead group key fix to release

## Goal

A relay on `release` holds no arrival entries for groups it no longer caches,
and a reader's wake no longer walks groups below its floor, so one publisher
that jumps to a far-ahead group key and keeps publishing below it cannot grow
a relay's memory or burn its shared runtime's CPU. Ships in a `moq-net` patch
in the next release.

## Plan

Decided 2026-11-17 (maintainer, [moq-dev/moq#9010](https://github.com/moq-dev/moq/pull/9010#issuecomment-6959731587)): ✅
backport the track fix to `release` as a separate PR after it lands on
`main`, per RELEASE.md (rejected: shipping it only with the cut).

Port the track half of the fix from
[moq-dev/moq#9044](https://github.com/moq-dev/moq/pull/9044) in
`rs/moq-net/src/model/track.rs`: arrival entries carry their own position, so
dead ones are compacted from anywhere once they outnumber the cached groups,
and the arrival walk moves a reader's cursor past leading entries it is never
handed (dead, or below its floor). `release` still indexes the queue by an
`offset`, so this is a hand port, not a cherry-pick. Reproduce first with the
PR's `a_far_ahead_key_pins_nothing_behind_it` test; if `release` cannot hit it,
delete this quest and say why in the PR.

The front's track-info cleanup stays on `main`: it is not part of the decision.

Public API: none. Wire: none.
