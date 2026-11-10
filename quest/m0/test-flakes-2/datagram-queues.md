# [XS] js/net's lite-07 datagram queue test holds up under load

## Goal

`js/net/src/lite/datagram-tracks.test.ts`'s "lite-07 bounds and prioritizes
independent createWritable datagram queues" passes on a loaded host, and its
`test.skip` is removed.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8276](https://github.com/moq-dev/moq/pull/8276)), under the
repository's flaky-test rule. It failed once during a full `just check` on
#8276's branch and passed 8 of 8 times alone and in later runs.

Suspected, not confirmed: the test asserts
`pair.server.sendStreams.datagrams` has two queues right after
`second.recv()`, but nothing orders the first track's queue before that
point, so under load it may not exist yet. Reproduce it first, under load as
the epic describes. Then wait on the event that creates the queue rather than
asserting at once. If the publisher can deliver on the second track before
creating the first track's queue when both were subscribed first, decide
whether that is a bug before changing only the test.

Public API: none. Wire: none.
