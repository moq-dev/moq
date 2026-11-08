# [XS] js/net's widened max-age re-drop test ends cleanly, not with a reset

## Goal

`js/net`'s "an update keeping the budget drops the stale groups again on
lite-07" cannot flake. It finishes the track only after the re-drop reply has
reached the subscriber, so the end arrives as a FIN, and it detects a missing
re-drop without a wall-clock deadline.

## Plan

Planned 2026-11-08 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8230](https://github.com/moq-dev/moq/pull/8230), whose iterate
saw it fail there and on unrelated branches, then pass on rerun.

The observed failure (8230's `Check`, job 128649627407) took 9.30 ms:
`recvGroup()` rejected with `reset stream: 1` (`StreamCode.Cancel`), thrown
from `js/net/src/lite/publisher.ts`'s `response()`. When the stream ends
while a reply write is blocked on the write budget, `response()` resets the
writable half with `Cancel`. Here the reader's priority-only update sends a
reply while the producer finishes, so the clean end can lose to the reset.
The test's 500 ms elapsed assert never ran.

The reset is intended:
[moq-dev/moq#8195](https://github.com/moq-dev/moq/pull/8195) rejected letting a
clean FIN override a pending reply write, since that write could block on flow
control for a peer that has left. So the race is in the test, which finishes
the producer right after `reader.update()` without waiting for the re-drop
reply. Fix the test, not the publisher: wait until the subscriber has received
the re-drop of groups 1 and 2 under the new update, then finish the producer.
That wait also replaces the 500 ms elapsed assert. Bound it on the `Manual`
clock, or by event, so a missing re-drop (which would otherwise wait out the
1.5 s grace) still fails while a CI pause cannot.

Public API: none. Wire: none.
