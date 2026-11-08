# [XS] js/net's widened max-age re-drop test runs on a mocked clock

## Goal

`js/net`'s "an update keeping the budget drops the stale groups again on
lite-07" passes on a loaded host. It proves the re-drop on a mocked clock,
not with a wall-clock deadline.

## Plan

Planned 2026-11-08 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8230](https://github.com/moq-dev/moq/pull/8230), whose
iterate saw it fail there and on unrelated branches, then pass on rerun.

The test (`js/net/src/lite/widened_max_age.test.ts`) reads `now()` before
finishing the track and asserts the reader's end arrives within 500 ms of
real time, as evidence that the stale groups were dropped again rather than
waited out for the 1.5 s grace. On a loaded host the mock transport's turns
alone can exceed 500 ms. Drive it with the `@moq/clock` `Manual` clock
instead: assert the end arrives before the clock reaches the grace, so a
missing re-drop fails deterministically. Check the file's other cases for
the same wall-clock assert.

Public API: none. Wire: none.
