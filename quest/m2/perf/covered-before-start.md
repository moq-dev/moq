# [S] Lite coverage checks before START are cheap

## Goal

Before SUBSCRIBE_START, the lite-07 publisher's coverage check stops
rebuilding a `RangeSet` from every skipped range and rescanning the covered
runs on each wake. A wake costs work proportional to what changed since the
last one, not to the skipped record's size.

## Plan

Found by [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995), which
made `covered` incremental after the start resolves. `self.skipped` is already
updated incrementally; the cost is the set rebuilt from it and the scan in
`track::Subscriber::covered`. Keep an incremental coverage summary or cursor
across wakes instead. `track_range_filling` never builds a `TrackRun` or
reaches this branch, so add a benchmark that drives the lite publisher with
no start resolved, swept over skipped ranges.

Public API: none.
