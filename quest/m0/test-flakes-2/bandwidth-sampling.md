# [XS] js/net's bandwidth sampling test holds up under load

## Goal

`js/net/src/connection/bandwidth.test.ts`'s "a connection samples the
transport only while a reservation reads the estimate" passes under load and
runs again.

## Plan

Planned 2026-11-08 (maintainer). It failed under load in three worktrees and
passes on a quiet main. It is quarantined with `test.skip` until this quest
fixes it.

The test counts `getStats` calls over 350 ms of `Manual` clock time and
expects exactly three 100 ms ticks while a reservation reads the estimate, and
none otherwise. Find which step outruns the mocked clock under load: a sample
loop started or stopped on a host task rather than within `advance`, or the
reservation's first read landing after the window opens. Fix it at the cause,
for example by having the test wait on the sampler's start/stop event before
counting, never with a longer window or a retry. Reproduce with
`bun test --rerun-each 200` under a loaded machine first. Then remove the
`test.skip` and delete this quest.

Public API: none. Wire: none.
