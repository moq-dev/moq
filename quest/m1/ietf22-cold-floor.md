# [S] A cold moq-transport-22 copy serves its absolute floor

## Goal

A relay copy that subscribes upstream over moq-transport-22 with an absolute
floor, such as `0..`, receives every group from the floor the publisher
still holds, not only the latest. `warm_backfill` in
`rs/moq-net/tests/integration` covers moq-transport-22 again.

## Plan

Decided 2026-10-29 (maintainer, `/quest-plan` of moq.pro's announce ramp
loss follow-ups): its own quest in m1, beside the other IETF fill and FETCH
quests. This quest owns the draft-22 absolute-floor backfill, and
[Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) excludes it.

Found while landing [#7168](https://github.com/moq-dev/moq/pull/7168), which
dropped moq-transport-22 from `warm_backfill` for this. On `main`, the copy
records start 0, but the publisher's cursor starts at the live edge, so only
the latest group arrives. Find which side loses the floor: the subscriber's
draft-22 SUBSCRIBE or fill mapping, or the publisher's resolution of an
absolute start. Fix it there, and keep draft 18 unchanged.

## Related

- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - the other draft-22 fill work in Rust
