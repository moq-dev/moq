# [S] Deterministic TS legs for H.265

## Goal

H.265 sources carrying a NAL HRD buffering period get the same group anchor
as H.264 under `moq export ts --sync`, so their redundant legs render every
common slot identically through complementary loss.

## Plan

Decided 2026-10-19 by the maintainer (#5864): the
[deterministic TS legs](/quest/m2/ts-hitless.md) anchor reads H.264's buffering
period SEI; H.265 lays its SEI out differently, so it keeps today's anchor
and counts overruns until this lands. Reuse the H.264 tests with an H.265
fixture an encoder wrote.

Public API: none.

## Required

- [Deterministic TS legs](/quest/m2/ts-hitless.md) - the HRD anchor this extends
