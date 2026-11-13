# [S] The instruction bench rejects cells measured on a busy host

## Goal

`just bench-profile instructions` never reports a number from a cell that other work on the host disturbed, or that did not deliver its workload: it reruns the cell, and fails the run if no clean window comes, so every figure in its summary is comparable with the others.
Today each cell records the load it ran under (`host_load_1m` in `bench/instructions.py`), its delivered rate, and its lost groups, and the summary prints them, but telling a clean cell from a contaminated one is left to the reader.

## Plan

Decided 2026-11-12 (maintainer, `/quest-plan`), from an agent memory note that recorded the workaround ("compare only quiet cells within one run") instead of a fix:

- **Scope.** ✅ Fix the bench (rejected: keep the workaround as a paragraph in `bench/README.md`).
- **Milestone.** ✅ the perf epic (rejected: m0, m1). Moved to lab 2026-11-12 (maintainer, /quest-audit): bench tooling only.

What was measured on the shared 24-CPU host, 2026-11-02 and 2026-11-03: instructions per delivered frame read 5 to 20% low once the one-minute load passed about 9, because a contended relay batches more frames per wakeup, and above about 20 cells also lost groups.
One revision read 142.7k under load and 169k to 175k quiet.
`quest gate` checks the load only when it admits the command, and the load swung from 2 to 250 during multi-hour bisects ([#7605](https://github.com/moq-dev/moq/pull/7605)).

Guidance, for the implementer to confirm:

- The load average cannot be the test. It counts the bench's own relay and generator, so a clean 16-worker cell reads higher than a contaminated 4-worker one, and no single threshold separates them. Measure the other work on the CPUs the bench pins its relay and generator to, such as those CPUs' busy time in `/proc/stat` less the bench's own. `/proc/pressure/cpu` is host-wide, so it has the same fault. Work on CPUs the bench does not use is not a reason to reject a cell.
- The one-minute average also lags: recorded cells at load 5.25 and 8.7 delivered 4 to 5% under the target rate with no lost groups (`.github/relay-cpu-bisect.md`). A cell more than 2% off its target rate in either direction, the band the bisect already accepted, or one that loses a group, is rejected whatever the host measure says; `bench/instructions.py` already has both numbers. A contended relay also overshoots as it drains a backlog: one recorded cell delivered 23.3k frames per second against a 20k target with no lost group and read 102.6k against 107.3k and 112.0k quiet (`.github/relay-cpu-bisect.json`, run nPd1Nb). Clean cells read up to 1% under, so any deviation at all is too strict.
- Bound the reruns, and say in the failure which cells never got a clean window.
- `bench/local.py` records `host_load` as well; decide whether it takes the same rule.
