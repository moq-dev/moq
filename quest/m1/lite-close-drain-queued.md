# [S] A lite close drain keeps the streams queued when the session closed

## Goal

When a `@moq/net` lite session drains after the peer's close, a group stream
the peer opened before closing is delivered even if it was still queued, not
yet accepted, when the close arrived, over real WebTransport as well as the
mock. A deterministic test forces the accept loop to yield in the middle of
the close drain and shows no stream is lost.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7386](https://github.com/moq-dev/moq/pull/7386).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- [moq-dev/moq#7386](https://github.com/moq-dev/moq/pull/7386) (merged) made JS accept loops yield on the shared `acceptBudget`
  (`js/net/src/stream.ts`), and the closing lite session now waits for the
  accept loop to take streams that arrived before the close. But the drain
  still reads only streams the transport delivers after `close()`, and on a
  real WebTransport incoming streams error when the session closes, so a
  stream queued at that moment is lost no matter what. This predates [moq-dev/moq#7386](https://github.com/moq-dev/moq/pull/7386).
- Reproduce over real WebTransport first. The receiver alone cannot rescue a
  stream the transport has already errored, so expect the fix to be on the
  closing side (not closing the session until the peer has taken its
  streams), which
  [moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) (merged) did
  for lite-05/06: `close()` now waits up to one second for the subscriber's
  FIN, sent after it read the tail (`js/net/src/lite/connection.ts:255` at
  be39188000). lite-01..04 keep the transport-ACK drain (its decision 1).
- Testability: the budget's 32-check counter is module-global, so a test
  cannot choose which accept yields. Make the budget per session or
  injectable (`@internal`) and add the mid-drain yield test.

Public API: none (`acceptBudget` is `@internal`). Wire: none.

## Related

- [@moq/net's close-drain tests survive load](/quest/m0/test-flakes-2/js-close-drain.md) - the same close-drain tests, flaking under load
