# [XS] @moq/net's boundary-sync test survives load

## Goal

"a version without a boundary syncs once the request is out"
(`js/net/src/lite/subscriber.test.ts`, around line 2895) passes on every full
`@moq/net` suite run.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). It failed once in a full run under load during
[moq-dev/moq#7371](https://github.com/moq-dev/moq/pull/7371), and passed 5 of 5
runs alone. Find the ordering it assumes, and drive it with the manual clock.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
