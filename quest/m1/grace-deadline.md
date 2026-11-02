# [S] A replaced copy's grace wakes its reader

## Goal

When a replaced copy's grace for a missing group runs out, a wall-clock deadline
wakes the waiting reader, so a clean end is reported on time instead of up to
one budget late.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7504](https://github.com/moq-dev/moq/pull/7504). It
predates that PR. Arm the grace's deadline wherever the grace starts in the
resume and miss paths (`rs/moq-net/src/model/resume.rs`, `track/miss.rs`), and
test it on a manual clock: the end arrives at the grace, not a budget later.
