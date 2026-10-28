# [S] Lite coverage checks before START are cheap

## Goal

Before SUBSCRIBE_START, the lite-07 publisher's `track::Subscriber::covered`
check stops rebuilding a set from everything skipped on every wake, so a wake
costs O(1) instead of O(skipped).

## Plan

Found by [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995), which
made `covered` incremental after the start resolves. Keep the skipped record
incremental from the first wake too, and cover it in the
`track_range_filling` benchmark.

Public API: none.
