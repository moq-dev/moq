# [S] js/net's lite-07 re-dropped re-offer test holds up under load

## Goal

`js/net/src/lite/publisher.test.ts`'s "lite draft-07: a re-offer dropped
again before it opens stays stale at the end" passes on a loaded host, and
its `test.skip` is removed. Update 3 is guaranteed to land before the
re-offer of group 1 opens, so the end still reports group 1 as stale.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete), under the repository's
flaky-test rule. Quarantined by
[moq-dev/moq#8333](https://github.com/moq-dev/moq/pull/8333) after the whole
`js/net` package failed it 1 of 30 times under 48 CPU burners on 24 cores,
first seen in [moq-dev/moq#8326](https://github.com/moq-dev/moq/pull/8326)'s
loaded runs.

The failure: after 22 ms, `holesToEnd` returned `[]` where
`[["drop", 1n, 1n, 3n]]` was expected (`publisher.test.ts:1572`). Suspected,
not confirmed: the write budget is already stubbed, but the test sends update
2, waits one `yieldNow()`, then sends update 3, assuming update 3 lands
before the re-offer opens; under load the re-offer is served first. Reproduce
it first. Then order the test on an event (the update's reply or the
re-offer's queueing), not a count of yields. If the publisher can serve a
re-offer an already-sent update dropped, that is a publisher bug: fix it there,
mirroring Rust's `a_redropped_reoffer_stays_stale_at_the_end_on_lite07`.

Public API: none. Wire: none.
