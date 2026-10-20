# [L] Finish controlled-time test coverage

## Goal

Remaining test-only and private clock consumers use controlled time, and the
workspace lint prevents new direct reads outside backends and measurements.

## Plan

The production and exported API migrations stay in m1. This follow-up owns
unrelated legacy test conversions, the workspace-wide lint ratchet, and the
final audit of harness exceptions. Real I/O synchronizes on events; only
watchdogs and performance measurements use wall time.

## Required

- [Controlled time](/quest/m1/time/README.md) - the clocks and production migrations
