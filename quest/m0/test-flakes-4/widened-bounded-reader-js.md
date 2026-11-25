# [S] A widening recovers a bounded JS reader's stale groups every run

## Goal

The `js/net` test "a widening recovers the stale groups for a bounded lite-07
reader" (`js/net/src/lite/widened_max_age.test.ts`) passes every run, fixed at
its cause.

## Plan

It fails on `main` about one run in nine, measured 2026-11-24 by the agent
merging main into [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529)
during workspace-wide JS runs; js/net alone passed 20 of 20 resume reruns.
Reproduce it under load first, record the failing assertion here, then fix
the cause: a race between the widening and the stale give-up is the likely
suspect, which a fake clock or an explicit event in the test should expose.
