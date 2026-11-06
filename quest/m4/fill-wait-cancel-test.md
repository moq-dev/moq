# [XS] A cancelled miss stops waiting for the relay's own fill

## Goal

A test proves that a relay's cache miss waiting on its subscription's own
fill stops waiting, and releases its task, once no reader wants the group or
the track is aborted.

## Plan

Found 2026-10-29 by review of [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135).
There, a miss for the joined group waits until the subscription's own request
has delivered or failed before sending a FETCH, and the wait also ends when no
reader wants the group or the track is aborted. That last part has no test:
the PR found no way for a test to observe a released task.

Decided 2026-10-29 (maintainer): m2. Add the smallest test-only hook that can
see the wait end (for example a counter of live waits in
`rs/moq-net/src/ietf/subscriber.rs`, compiled only for tests), then cover a
cancelled reader and an aborted track. Rejected the same day: a time limit
after which the relay FETCHes the joined group's start anyway, since losing
that group is the documented contract and a late FETCH risks the duplicate
group #7135 fixed.

Public API: none. Wire: none.
