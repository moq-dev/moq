# [S] Re-serving a claimed path keeps the session

## Goal

A JS publisher that answers a claimed path's request with a fresh broadcast
whose first group is above 0 (continuing an earlier broadcast's group
sequence) keeps its session through a relay on `moq-lite-07-wip`; today the
relay closes the publisher's connection with `invalid control encoding`.

## Plan

Seen 2026-11-03 in Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)), which
worked around it by replacing tracks in the same broadcast instead. Find
whether [moq-dev/moq#7851](https://github.com/moq-dev/moq/pull/7851)'s FETCH
budget-boundary fix explains the fresh-broadcast failure before proposing a
separate codec change or regression test.

On 2026-11-04, a candidate test
([checkpoint](https://github.com/moq-dev/moq/blob/425d4046c6b0bd94677a4bc4de715dd257b16856/test/interop/claims.ts)) passed against
`main` at `ffcb7cd9748422daa30e935ec464108b5a8ebb71`, before any encoder or
decoder change. It uses a real `moq-relay` on lite-07, keeps the publisher
connection and claim instance, closes the first broadcast and track, opens a
fresh reader connection, and answers the same path with a fresh broadcast
whose first group is 1. Groups begin only after upstream demand. It uses the
reporting app's relative-zero range, ascending order, unlimited subscriber
delay and publisher retention, refresh 1, and publisher priority 1.

Additional local variants also passed: first answer at group 1, groups buffered
before serving, an absolute group-1 subscription floor, and a fresh route
instance between answers. The control codecs have not been changed, because
none of these cases reproduces the reported invalid encoding by itself.

The replacement investigation in
[moq-dev/moq#7851](https://github.com/moq-dev/moq/pull/7851) subsequently traced a
matching publisher-session failure to a missing-group FETCH, rather than group
sequence encoding. An unlimited reader causes Rust to send a FETCH Max Delay
of `Number.MAX_SAFE_INTEGER`, encoded with its optional-field offset as
9007199254740992. JS reads that encoded value with `r.u53()` before subtracting
the offset, and refuses it. That PR owns the FETCH boundary fix and its
replacement reproducer; this quest must not duplicate that implementation.
The candidate does not trigger the missing-group FETCH, so it was removed from
the final diff instead of adding an automatic test that misses the known fault.

## Waiting

- moq-dev/moq: finish the FETCH budget-boundary fix and its regression coverage
  in [moq-dev/moq#7851](https://github.com/moq-dev/moq/pull/7851), then check whether
  it covers the original fresh-broadcast re-serve failure too.
- Maintainer: after that check, retire this quest through the shared fix if it
  explains the original failure (recommended), or retain it for a distinct
  reproducer. The passing candidate is not evidence that the original bug has
  been fixed.
