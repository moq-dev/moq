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

Start after [Watch](/quest/m1/audio-jitter-target/watch.md) lands
([moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) time-stretches
the browser's audio ring), then grade the next scheduled nightlies (never
dispatch one). If the rows pass, delete this quest. Otherwise find what
stalls Opus that the stretch does not cover and fix it there; do not loosen
the budget to pass.

Public API: none expected. Wire: none.

## Required

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the browser time-stretches its audio ring, which may already remove these stalls
