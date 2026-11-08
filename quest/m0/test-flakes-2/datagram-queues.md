# [XS] js/net's lite-07 datagram queue test holds up under load

## Goal

`js/net/src/lite/datagram-tracks.test.ts`'s "lite-07 bounds and prioritizes
independent createWritable datagram queues" passes under load and runs again.

## Plan

Planned 2026-11-08 (maintainer). The test, from
[moq-dev/moq#8276](https://github.com/moq-dev/moq/pull/8276), failed under load.
It is quarantined with `test.skip` until this quest fixes it; #8276 must land
it skipped or fixed.

Suspects: the test asserts `sendStreams.datagrams` has two queues right after
the first `recv`, and that the oversized appends leave sequence 1 as the next
read, both of which depend on the publisher's write side having run before the
assertion. Under load a write-budget yield can reorder that. Wait on the
event the assertion depends on (the queue's creation, the priority's update
reaching the queue), never a longer timeout or a retry. Reproduce with
`bun test --rerun-each 200` under a loaded machine first. Then remove the
`test.skip` and delete this quest.

Public API: none. Wire: none.
