# [S] Chromium Opus rows pass a measured stalled-share budget

## Goal

Nightly's Audio quality browser rows for Chromium Opus pass a `stalled_share`
budget measured with the browser's time stretch every night. Today a
different row fails each night at 0.014 against 0.01: `opus-48000-bursty` on
2026-11-08 and `opus-48000-wide` on 2026-11-07.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete from the triage of Nightly
runs [42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523)
and [42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545)).
✅ A child of this epic, m1 (rejected: leaving it to the watch quest's
grading).

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058), which
time-stretches the browser's audio ring, merged on 2026-11-09 and may already
remove these stalls. Grade the scheduled nightlies after it (never dispatch
one). If the rows pass, delete this quest.

Decided 2026-11-09 (maintainer, /quest-plan of the
[epic's model](/quest/m1/audio-jitter-target/README.md)): zero stalls is not
a goal, so a row over 0.01 is not by itself a defect to hunt. This replaces
"do not loosen the budget to pass". The 0.01 ceiling was measured before the
stretch, so if the rows still exceed it, re-measure `stalled_share` for the
Chromium Opus rows by the rule in `budgets.json`'s note from seven nights
after #8058. Fix a cause only when a stall is one the player should have
played through: audio that had arrived, or a refill longer than the model's
cushion. [Calibrate](/quest/m1/audio-jitter-target/calibrate.md) re-measures
every ceiling again once the model lands.

Public API: none expected. Wire: none.
