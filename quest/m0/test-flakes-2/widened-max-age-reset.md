# [XS] js/net's widened max-age re-drop test ends cleanly, not with a reset

## Goal

`js/net`'s "an update keeping the budget drops the stale groups again on
lite-07" cannot flake. A reply still pending when the track finishes ends
the reader's track cleanly, never with a `Cancel` reset, and the re-drop
check does not depend on wall-clock time.

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

[moq-dev/moq#8195](https://github.com/moq-dev/moq/pull/8195), which landed
after that run, changed how lite-07 replies yield on the write budget, so it
may already have cured this. A clean rerun sample cannot show that. Add a
deterministic test that holds a reply pending on the write budget while the
track finishes, and asserts the subscriber sees a clean end, not `Cancel`. If
it passes on main, it lands as the regression test. If it fails, fix the race
in the publisher so the end still arrives as a FIN.

Also move the test's 500 ms elapsed assert onto the `Manual` clock, or an
event-based check, so a CI pause cannot fail it, while a missing re-drop
(which would wait out the 1.5 s grace) still does.

Public API: none. Wire: none.
