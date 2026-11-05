# [S] An Absolute join never races an older-history FETCH

## Goal

A moq-transport subscriber whose joining fill starts at an Absolute group,
while an older-history FETCH for the same track is in flight, gets every
group of both. A later join is never degraded. Otherwise, a reproduction
shows that main can't reach the collision, and this quest is deleted.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680)): milestone m2,
reproduce first.

- #7680's local review, while backporting the joined-head FETCH fix,
  flagged a possible pre-existing collision on main. An Absolute joining
  fill and an older-history FETCH share state, and the collision degrades a
  later join. It is unverified, and #7680 didn't chase it.
- Main's model differs from `release`: range fills and `Filter::Absolute`
  live in `rs/moq-net/src/ietf/subscriber.rs`. Write a deterministic test
  that interleaves an older-history FETCH with an Absolute join on drafts
  18, 20, and 22. If the test passes, delete the quest with the test kept as
  coverage.
- Check [the fill redesign](/quest/m2/ietf-fill-refill.md) first. It deletes
  the `Fill` handoff and may make this moot. If it lands first, rerun the
  test against it.

Public API: none expected. Wire: none.

## Related

- [A moq-transport group head fills its hole in place](/quest/m2/ietf-fill-refill.md) - the fill redesign that may remove the race
