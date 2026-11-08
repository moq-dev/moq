# [XS] js/net's widened max-age re-drop test ends cleanly, not with a reset

## Goal

`js/net`'s "an update keeping the budget drops the stale groups again on
lite-07" passes under `--rerun-each 200`. The reader's track ends cleanly
after the producer finishes, never with a `Cancel` reset.

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
after that run, changed how lite-07 replies yield on the write budget. First
check whether main still fails under `--rerun-each 200`, and whether the
unrelated-branch failures were the same reset. If main is clean, delete this
quest. Otherwise fix the race in the publisher, so a reply blocked when the
track ends cleanly still lets the end arrive as a FIN, with a test that fails
without the fix. Moving the 500 ms wall-clock assert onto the `Manual` clock is
optional hardening, since that assert has not failed.

Public API: none. Wire: none.
