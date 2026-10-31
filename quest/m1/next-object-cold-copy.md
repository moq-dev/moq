# [S] A Next Object join on a cold Rust copy starts at object 0

## Goal

A downstream Next Object join that reaches a Rust relay after the upstream
reported a group, but before any of that group's frames arrived, receives the
group from its first frame received, never skipping frames.

## Plan

Found while landing [moq-dev/moq#6815](https://github.com/moq-dev/moq/pull/6815)
and not yet reproduced. `report_object` keeps the exact upstream Object ID
`k`, while readers count frames from 0, so the join may start at frame `k+1`
and skip the first `k+1` frames the relay then receives. That loses data,
unlike #6815's accepted precision trade-offs. JS avoids it since #6815's fix
for synthetic group-head reports (a separate `reportedGroup` hook).

Reproduce it first with a Rust test that reports `{g, k}` upstream, then joins
with Next Object before any frame of `g` arrives. If it reproduces, keep the
reported Object ID out of the reader's frame index, as JS does.

Public API: none. Wire: none.
