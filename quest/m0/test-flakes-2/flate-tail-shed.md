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
Check job, `ci: test js failed`). That PR changes moq-mux's TS exporter, `rs/moq-cli`,
`js/net/src/lite/datagram-tracks.test.ts`, and quest docs; none of it touches
flate or its test, though a `js/net` test-side cause is not ruled out.
Both variants are quarantined with `test.skip`, since they share the loop.

Baseline (unloaded, pre-quarantine file copied aside): both variants together
ran in 1198 ms on the maintainer's dev host; a reviewer measured ~2.1 s per
variant on theirs, with `plain=true` only ~2% slower than `plain=false`. So the
second `toEqual` is not the cost, `plain=false` is no cheaper, and CI's 5697 ms
is a ~2.7x loaded slowdown, not a cliff.

Lead (unverified): the test is CPU-bound in its 100 iterations that compress
and inflate a 1 MiB frame. If profiling confirms it, cut the work volume while
keeping what the test proves (the cache stays bounded and the slow reader is
evicted), such as smaller frames against a proportionally smaller cache bound,
leaving enough headroom that a ~3x loaded slowdown still fits; if
deflate/inflate in the code under test is the slow part, fix that.

Reproduce first: time the case alone and under CPU contention
(`--rerun-each`) before changing it.

Public API: none. Wire: none.
