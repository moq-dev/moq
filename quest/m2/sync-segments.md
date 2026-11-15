# [M] moq_mux::sync resets at a segment transition, as JS does

## Goal

`moq_mux::sync` handles a declared end and its replacement media with one
rule that Rust and JS share, and its state follows REVIEW.md's State rules:
one enum per lifecycle, one lock per value.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
Segment transitions in `rs/moq-mux/src/sync.rs` keep parallel `members`,
`resumed`, and `plans` arrays in `Pending`, and one track's segment state sits
behind two locks.
[moq-dev/moq#8589](https://github.com/moq-dev/moq/pull/8589) and
[moq-dev/moq#8513](https://github.com/moq-dev/moq/pull/8513) patched it, and
#8589 says JS needs no mirror because `js/watch/src/sync.ts` has no segment
transitions, so Rust runs a third model the deleted mux-sync quest said not
to invent.

Coordinate with
[moq-dev/moq#8749](https://github.com/moq-dev/moq/pull/8749), which changes
the same clock.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) delete the transitions and reset the way JS does (rejected: (b) one enum per role, ported to JS).

Public API: possibly `moq_mux::sync`'s transition surface, unpublished.
Wire: none.

