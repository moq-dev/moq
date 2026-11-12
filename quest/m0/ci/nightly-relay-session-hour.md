# [S] Nightly's relay session benchmarks fit their hour

## Goal

`Nightly (Relay session benchmarks)` (`just gh nightly rust-relay`, a paired
group that measures this commit and the previous Nightly's) finishes inside
its 60-minute `timeout-minutes` on a scheduled run and keeps its paired
regression gate.

## Plan

The job was cancelled at the timeout on the 2026-11-08, 11-09, and 11-10
scheduled runs. Unlike the other cancelled jobs it does not hang:
[the 11-10 run](https://github.com/moq-dev/moq/actions/runs/42842827718/job/129922278379)
was still finishing `relay_session_join_broadcasts` cells seconds before the
cancel. The scheduled runs from 11-01 to 11-05 failed inside the workload after
14 to 20 minutes, the
[11-01 job](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517800)
with `Evicted`. The
[11-06](https://github.com/moq-dev/moq/actions/runs/42236107954) and
[11-07](https://github.com/moq-dev/moq/actions/runs/42377867523) runs
completed the sweep in about 25 and 23 minutes (their jobs failed later, at
`Fail on paired regressions`). Find what grew between those sweeps and the
60-minute cancellations from 11-08, then trim it or split the group across
matrix entries, as the Rust benchmark tests did. The sweep still covers both
fan-out axes.
