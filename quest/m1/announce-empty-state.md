# [S] Watch, room, and the demo show when nothing is live

## Goal

Once an announcement stream reports `live` with nothing announced, `js/watch`,
`js/room`, and `demo/web` show a "no broadcasts" state instead of a spinner
that never resolves, and never a false empty state before the first session
answered. A connection that fails shows an error state, not an empty one.

## Plan

- Today each consumer skips the `live` marker (`demo/web/src/index.ts`,
  `js/room/src/room.ts`, `js/watch/src/broadcast.ts`, `js/moq-boy`). Track it
  next to the announced set: loading before `live`, the empty state after it
  with nothing announced, and an error state when the connection fails.
- Libraries expose the state as a signal; wording stays in the demo.

Public API: any state signal on `@moq/room` or `@moq/watch` is additive.
Wire: none.

## Required

- [Page-load marker](/quest/m1/announce-page-load.md) - otherwise the empty state flashes on page load before the first connection replays
