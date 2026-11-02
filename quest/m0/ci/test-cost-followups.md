# [M] The next test-CPU cuts

## Goal

The test costs [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)
found but left are cut at their source, with coverage kept.

## Plan

Left by #6987 (halve the slowest tests' CPU):

- Encode the recovery clips once per run instead of per test.
- Use `tracing-test`'s `no-env-filter` where its filter costs time.
- Make the moq-stats oversize tests cheap.
- Make `a_fill_refused_group_by_group_stays_linear` cheap without losing what
  it proves.

Measure each before and after, as #6987 did. The export idle poll and mesh
session cost already have quests in `quest/m2/perf/`, and
[moq-dev/moq#7075](https://github.com/moq-dev/moq/pull/7075) already decodes a
moq-quic padding run as one frame.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m0 CI quest / drop.

Public API: none. Wire: none.
