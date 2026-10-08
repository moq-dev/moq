# [S] A JS consumer's first group is never held behind a hole

## Goal

`@moq/hang`'s container `Consumer` delivers the first group that carries
frames as soon as it arrives, whatever died before it. Today, if the first
group dies before a frame is read and the next arriving group is not adjacent
(16 reset, then 18), `next()` delivers nothing until a later group lets
`#checkMaxDelay` skip, which can cost a whole group (2.5 s of video) before the
first picture after a reattach.

## Plan

Root cause (verified 2026-10-06): a hole only exists relative to presented
content, but `next()`'s promotion guard in
`js/hang/src/container/consumer.ts` needs `maxDelay === 0` or a defined
`#presentedEnd` to promote a head that is not contiguous with `#active`.
Two paths reach it with nothing presented:

- The reporter's: the first group is reset empty and `#runGroup`'s finally
  moves `#active` to `sequence + 1` with nothing buffered.
- An empty group adjacent to the dead first group closes cleanly, and
  `next()`'s done branch advances `#active` again (16 open, 17 empty and
  closed, 16 reset, then 19).

Decided 2026-10-06: fix it in the promotion guard, the one place every path
converges, not in each site that moves `#active` (the reporter's finally
fix misses the second path). While nothing has been presented, the head is
promotable. Keep this out of `skipHole`, so startup reports no
discontinuity, matching the "start with the first group" policy.

Open for the implementer: which field means "nothing presented".
`#presentedEnd` can stay undefined after frames were delivered when
`#checkMaxDelay` drops the delivered group without `#recordPresented`;
`#deliveredGroup === undefined` may be the sounder predicate.

Tests: both paths above, as bun tests with mocked or flushed time, deliver
the live group from its keyframe with no warning.

Public API: none. Wire: none.

## Closes

- [#4951](https://github.com/moq-dev/moq/issues/4951) - a consumer whose first group dies empty parks its cursor on a group that never comes
