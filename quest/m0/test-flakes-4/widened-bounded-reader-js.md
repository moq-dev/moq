# [S] A widening recovers a bounded JS reader's stale groups every run

## Goal

The `js/net` test "a widening recovers the stale groups for a bounded lite-07
reader" (`js/net/src/lite/widened_max_age.test.ts`) passes every run, fixed at
its cause.

## Plan

It fails on `main` about one run in nine, measured 2026-11-24 by the agent
merging main into [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529)
during workspace-wide JS runs; js/net alone passed 20 of 20 resume reruns.
Reproduce it under load first, then fix the cause: a race between the
widening and the stale give-up is the likely suspect, which a fake clock or an
explicit event in the test should expose.

It also failed Nightly
[45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151)
(2026-11-23, `b29b90be29`, `Nightly (JS coverage)`), which then skipped
coverage for every JS package after `js/net`; the next night's passed:

```text
js/net/src/lite/widened_max_age.test.ts:165:36
(fail) a widening recovers the stale groups for a bounded lite-07 reader
Expected: [3n, 4n]
Received: [1n, 3n]
```

The test writes groups 1 to 4 in one synchronous loop after subscribing with
a 1500 ms max age, so group 1 can be served before group 4 makes it stale.
