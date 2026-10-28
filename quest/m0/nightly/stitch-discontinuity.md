# [M] A stitched restart signals every TS discontinuity

## Goal

A restarted publisher exported with stitching signals the program break on every affected PID.

## Plan

[The CLI-export job](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001566) fails
`export::a_restarted_publisher_is_a_program_switch_with_stitch` at
"every PID flags the break": expected PIDs `[256, 257, 17]`, found none.

Reproduce on current main and trace the replacement program through the
follower and transport-stream muxer. Fix discontinuity signaling at its source
without weakening the end-to-end assertion. Coordinate with
[moq-dev/moq#7049](https://github.com/moq-dev/moq/pull/7049), which changes
how a stitched switch drops the old buffered tail; it does not claim this
PID-signaling failure.

Public API: none expected. Wire: none expected.
