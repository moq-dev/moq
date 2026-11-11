# [S] The JS subscribe-drop benchmark finishes

## Goal

`just js bench subscribe-drop` (`js/net/bench/subscribe-drop.ts`) finishes and
prints its rows, and the root cause of its hang is fixed, so
`Nightly (JS benchmarks)` finishes again.

## Plan

`Nightly (JS benchmarks)` was cancelled at its 60-minute timeout on the
2026-11-08, 11-09, and 11-10 scheduled runs, where it once took about ten
minutes. In
[the 11-10 run](https://github.com/moq-dev/moq/actions/runs/42842827718/job/129922278808)
`subscribe-drop.ts` printed its CSV header
(`publishers,subscribers,groups,drop_us`) and nothing for the job's last 57
minutes, and the benchmarks after it never ran. Reproduce it locally; if the
hang is in `@moq/net` rather than the benchmark, a test in the merge gate
covers it.

## Related

- [session_delivery_compression finishes on lite-07](/quest/m0/lite07-compression-bench-hang.md) - another Nightly benchmark that hangs
