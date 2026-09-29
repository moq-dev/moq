# [S] A JS fetch can be cancelled

## Goal

A `@moq/net` caller can abandon one pending group fetch without closing the
track or session, the follow-up #4357 left.

## Plan

Decided 2026-09-28: `FetchGroupOptions` (`js/net/src/track.ts`) gains
`signal?: AbortSignal`, the standard JS idiom, and additive.

- Thread it through `js/net/src/broadcast.ts` and both
  `lite/subscriber.ts` and `ietf/subscriber.ts`.
- Fetches for the same group share one stream (`lite/subscriber.ts`), so an
  abort releases this caller's share and rejects its promise with the
  signal's reason. The stream is cancelled only when the last sharer leaves.
- An already-aborted signal rejects before anything is sent.
- Tests: one of two sharers aborts and the other still receives the group;
  the last sharer aborting cancels the stream.
- Document the option where fetch is documented under `doc/`.

Public API: additive `FetchGroupOptions.signal`. Wire: none new; the
existing cancel path.
