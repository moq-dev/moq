# [S] Flate's tail-shed test passes under load

## Goal

`compressed tails shed dictionary prefixes without retaining inflated history`
in `js/net/src/flate/group.test.ts`, both `plain` variants, passes reliably
under load, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules, never by raising the test
timeout. Restore both cases with every assertion when it is fixed.

## Plan

Evidence: the `plain=true` case timed out at bun's 5000 ms default in the
merge-group batch for [moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981)
([run 42299125466](https://github.com/moq-dev/moq/actions/runs/42299125466),
Check job, `ci: test js failed`). That PR only touches moq-mux's TS exporter.
Both variants are quarantined with `test.skip`, since they share the loop.

Lead (unverified): the test is CPU-bound, not waiting on anything. Each of 100
iterations compresses and inflates a 1 MiB frame, then compares 1 MiB arrays
with `toEqual` once (`plain=false`) or twice (`plain=true`). About 100 MiB of
deflate plus up to 200 element-wise MiB comparisons can approach 5 s on a
loaded runner. If profiling confirms it, shrink the work while keeping what the
test proves (the cache stays bounded and the slow reader is evicted), for
example smaller frames against a proportionally smaller cache bound, or a
cheaper equality check; if the code under test is the slow part, fix that.

Reproduce first: time the case alone and under CPU contention
(`--rerun-each`) before changing it.

Public API: none. Wire: none.
