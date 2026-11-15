# [S] A peer's listing churn yields to the host's tasks, unquarantined

## Goal

`js/net`'s "a flood of a peer's listing churn yields to the host's tasks"
(`js/net/src/lite/tracks.test.ts`) runs again and passes under a loaded
`just check`, fixed at its cause on a mocked clock or an awaited event, never
with a looser budget.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md)'s
first pass: [moq-dev/moq#8355](https://github.com/moq-dev/moq/pull/8355)
skipped it after it timed out at 5 s in 2 of 30 full `js/net` runs under 48
CPU burners, while that PR fixed a different test, so no quest owned the
quarantine. Reproduce under the same load first, then decide whether the
budget loop or the test's wait is at fault, following this epic's rules.

Public API: none. Wire: none.
