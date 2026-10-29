# [XS] JS joining FETCH times out a pending SUBSCRIBE

## Goal

A test shows that a JS joining FETCH whose SUBSCRIBE is still being set up
at the 10 s request deadline is refused with TIMEOUT.

## Plan

[moq-dev/moq#6945](https://github.com/moq-dev/moq/pull/6945) added the wait
but left this path untested, for lack of a way to hold a subscription
pending in a test. Hold the SUBSCRIBE in setup with a publisher-side hook that never answers,
and drive the deadline on the `Manual` clock.

Public API: none.
