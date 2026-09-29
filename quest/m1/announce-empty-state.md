# [S] Watch, room, and the demo show when nothing is live

## Goal

`js/watch`, `js/room`, and `demo/web` follow the announcement stream's
`live`/`offline` toggle instead of a spinner that never resolves: loading
before the first `live`, a "no broadcasts" state after `live` with nothing
announced, and a reconnecting state on `offline` that keeps the held list on
screen. Never an empty state before a connection has answered.

## Plan

- Today each consumer skips the `live` marker (`demo/web/src/index.ts`,
  `js/room/src/room.ts`, `js/moq-boy`). Track the toggle next to the
  announced set.
- A connection that fails keeps the stream not-live, so the page stays in
  loading or reconnecting; show the connection's own error or retry status
  there (`Reload.status`, or the rejected one-shot `connect`), never the empty
  state.
- `js/watch` is different: its main broadcast waits on
  `origin.request(name, { announced: true })` in `#runBroadcast`, and its
  announcement stream opens only for relative catalog references. The named
  request needs its own caught-up-and-absent and offline states, driven by
  the same toggle, for watch to show "not live" or "reconnecting" instead of
  waiting.
- Libraries expose the state as a signal; wording stays in the demo.
- Tests in `js/room/src/room.test.ts` and `js/watch/src/broadcast.test.ts`:
  loading before `live`, empty once live with nothing announced, reconnecting
  on `offline` with the list kept, and never empty when the connection fails.

Public API: any state signal on `@moq/room` or `@moq/watch` is additive.
Wire: none.

## Required

- [Live and offline](/quest/m1/announce-offline.md) - the toggle these states are driven by
