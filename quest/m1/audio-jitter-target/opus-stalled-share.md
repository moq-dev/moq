# [S] Chromium Opus rows stay under the stalled-share budget

## Goal

Nightly's Audio quality browser rows for Chromium Opus pass their
`stalled_share` budget (0.01) every night. Today a different row fails each
night at 0.014: `opus-48000-bursty` on 2026-11-08 and `opus-48000-wide` on
2026-11-07.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete from the triage of Nightly
runs [42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523)
and [42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545)).
✅ A child of this epic, m1 (rejected: leaving it to the watch quest's
grading).

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058), which
time-stretches the browser's audio ring, merged on 2026-11-09 and may already
remove these stalls. Grade the scheduled nightlies after it (never dispatch
one). If the rows pass, delete this quest. Otherwise find what stalls Opus
that the stretch does not cover and fix it there; do not loosen the budget to
pass.

Public API: none expected. Wire: none.
