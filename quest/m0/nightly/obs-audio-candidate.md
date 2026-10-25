# [S] OBS audio-candidate test passes under TSan

## Goal

`cpp/obs/test/moq-source-test.cpp`'s "failed audio candidate preserves
playback" passes under OBS TSan every night. On 2026-10-25 its check
`ctx->connection->audio && !ctx->connection->pending_audio` failed with no
TSan report.

## Plan

The cause is not known yet. It is not the counter racing the clear: the
"Audio track error" log that bumps `g_candidate_failures` and the
`pending_audio` clear both run under `ctx->mutex`, which the check then takes.
But the counter is global, also bumped by any "Failed to resolve" or "Failed
to subscribe", and the check also needs the active `audio` to survive.
Reproduce under TSan first, find which conjunct fails and which track and
error bumped the counter, then fix that cause. If the wait is too broad, make
it wait on the replacement's own failure. Do not add a retry or a sleep.

Planned 2026-10-25 (maintainer) from the triage of Nightly run
[40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303).

Public API: none. Wire: none.
