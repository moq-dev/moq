# [XS] OBS audio-candidate test waits for its condition

## Goal

`cpp/obs/test/moq-source-test.cpp`'s "failed audio candidate preserves
playback" passes under OBS TSan every night. On 2026-10-25 its check
`ctx->connection->audio && !ctx->connection->pending_audio` failed with no
TSan report.

## Plan

The test returns once the failure counter increments, but `moq-source.cpp`
clears `pending_audio` later, so the check races the clear. Make the test
wait on the event that clears `pending_audio` (or on a "candidate dropped"
signal) instead of the counter. Do not add a retry or a sleep. Before
changing anything, rerun the TSan job once to confirm it is this race.

Planned 2026-10-25 (maintainer) from the triage of Nightly run
[40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303).

Public API: none. Wire: none.
