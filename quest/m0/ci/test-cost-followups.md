# [M] The next test-CPU cuts

## Goal

The test costs [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)
found but left are cut at their source, with coverage kept.

## Plan

The recovery cache, tracing isolation, and cheap stats oversize tests landed in
[moq-dev/moq#7074](https://github.com/moq-dev/moq/pull/7074),
[moq-dev/moq#7071](https://github.com/moq-dev/moq/pull/7071), and
[moq-dev/moq#7072](https://github.com/moq-dev/moq/pull/7072).

[moq-dev/moq#7549](https://github.com/moq-dev/moq/pull/7549) implements the remaining
refused-fill cut: direct state assertions in the fast test and a nightly scaling
benchmark that retains the 100,000-refusal workload. Its measurements verify all
four areas, and its full local gate passes. Complete the required cross-vendor
local review, address its findings, and remove this quest and its links when the
PR is ready. The review could not start because the Claude account hit its weekly
limit; the PR records the handoff and alternatives.

The export idle poll and mesh session cost already have quests in `quest/m2/perf/`, and
[moq-dev/moq#7075](https://github.com/moq-dev/moq/pull/7075) already decodes a
moq-quic padding run as one frame.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m0 CI quest / drop.

Public API: none. Wire: none.
