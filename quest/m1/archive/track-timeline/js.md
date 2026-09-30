# [M] JS per-track timelines

## Goal

`@moq/hang` publishes one timeline per track with the same records, cuts, and
catalog `archive` map as Rust, and parses them identically.

## Plan

Port the landed Rust shape to `js/hang/src/timeline.ts` and its catalog schema,
replacing the aligned timeline and cross-track pacing rather than keeping both.
Cover the same cut rules and a static catalog outliving other tracks' records,
and check the records against Rust output in the interop suite.

## Required

- [Rust per-track timelines](/quest/m1/archive/track-timeline/core.md) - the format this mirrors
