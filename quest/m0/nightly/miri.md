# [XS] Nightly's Miri job stays inside its time budget

## Goal

Nightly's `Tests (rs miri)` entry passes and finishes well inside the job's
60-minute timeout on the 4-core ARM runner, and no single Miri test comes near
nextest's 15-minute `miri` profile cap, with headroom recorded from a real
Nightly run.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7315](https://github.com/moq-dev/moq/pull/7315)
(merged), which finished the Miri for in-memory unsafe quest.

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from [moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)):
moved under [Nightly passes on main](/quest/m0/nightly/README.md), since the
Miri job keeps the m0 Nightly red. Do not dispatch Nightly by hand for the
timings.

The first scheduled run with `rust-src` ([moq-dev/moq#7812](https://github.com/moq-dev/moq/pull/7812)),
[run 41852517544](https://github.com/moq-dev/moq/actions/runs/41852517544/job/126652295876)
on 2026-11-04, ran only the strict pass: two kio `task::` tests failed, and
`sh/rs/miri.sh` stops before the epoch pass when one fails.
[moq-dev/moq#7895](https://github.com/moq-dev/moq/pull/7895) fixed them and
shrank the slowest Miri workloads. What that run recorded:

- Job 19 min 22 s; tests start 6 min in (toolchain, cache, a 2 min 20 s build).
- Strict pass: 228 tests in 800 s (13.3 min). Slowest
  `model::frame::tests::a_delivery_attaching_mid_write_publishes_one_payload`
  372 s (SLOW at 300 s; about half was its payload fill, now cheap), then kio's
  `clock::sim::tests::a_large_task_set_counts_each_child_alone` 212 s (now one
  child past the 4096 Ready limit instead of 10,000: 56 s locally, from 280 s)
  and `task::tests::concurrent_wakes_are_never_lost` 188 s, against the 900 s cap.
- All 17 `model::frame::` tests ran and passed, including the two
  single-delivery tests from [moq-dev/moq#7508](https://github.com/moq-dev/moq/pull/7508);
  the selection needs no change.

The epoch pass has no ARM timing yet. On a loaded 24-core desktop it took 370 s
for 97 tests, with `model::group::test::a_group_has_no_frame_cap` (10,000
writes at about 30 ms each under Miri) past SLOW; #7895 ignores that test under
Miri, since it still runs natively in the merge gate.

Remaining: read the next scheduled run's log and record the job duration, both
pass durations, the slowest test, and headroom against both budgets. If the job
nears 60 minutes, split the strict and epoch passes into separate matrix
entries rather than raising the timeout, and keep nextest's 15-minute per-test
cap. A test past SLOW gets a smaller Miri workload.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: After [moq-dev/moq#7895](https://github.com/moq-dev/moq/pull/7895)
  lands, the next scheduled Nightly on `main` must pass `Tests (rs miri)` on
  `ubuntu-24.04-arm` with both passes. Read its log
  (`gh run view <run> --repo moq-dev/moq --log`) and record the timings above;
  local runs do not clear this wait.
