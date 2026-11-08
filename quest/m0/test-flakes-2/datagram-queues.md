# [XS] js/net's lite-07 datagram queue test holds up under load

## Goal

`js/net/src/lite/datagram-tracks.test.ts`'s "lite-07 bounds and prioritizes
independent createWritable datagram queues" passes under load and runs again.

## Plan

Planned 2026-11-08 (maintainer). The test, from
[moq-dev/moq#7846](https://github.com/moq-dev/moq/pull/7846), failed under load,
and was quarantined by [moq-dev/moq#8287](https://github.com/moq-dev/moq/pull/8287).

Not yet reproduced (2026-11-08, `/quest-start`). Runs, all on the un-skipped test:

- `--rerun-each 200` with the test filtered, idle: 200 pass.
- The same under about 40 busy-loop burners on 24 cores: 200 pass.
- The whole file, `--rerun-each 100`, under load near 190: 3900 pass.
- Every `Budget.poll()` forced to yield (the write-budget yield the suspect
  names), file `--rerun-each 50`: 1950 pass.
- The whole `js/net` package, five runs under 48 burners: the test passes in
  all five. Other tests failed in two runs (the `lite draft-07` re-offer test,
  in `publisher.test.ts`, and a compressed-frame test); those are not this
  quest's.

Read against the code (`js/net/src/lite/publisher.ts` lite-07 `runDatagrams`,
`#runDatagrams`, `js/net/src/util/yield.ts`): the write-budget yield sits only
in the subscription's update loop, before it decodes the next update. The
assertions after `second.recv()` (two send queues, the queue's send order, the
sequence-1 read) follow from ordering, not from timing. The one implicit step is
that the audio queue is opened after its `DatagramsInfo` write. No event exposes
that open, so nothing here can wait on it directly. That is a candidate cause, not
a confirmed one.

Next, if the failure returns: reproduce on the failing machine's exact load, then
wait on the queue's creation or the priority's arrival at the queue, never a
longer timeout or a retry.

Public API: none. Wire: none.

## Waiting

- Maintainer: whether to keep the lite-07 test un-skipped without a reproduced
  cause. This PR removes its `test.skip`. Recommendation: keep it un-skipped. It
  passed about 6,000 loaded runs with no failure, and a skip hides coverage. If
  it fails again, re-quarantine it and reopen this quest with that run's log.
