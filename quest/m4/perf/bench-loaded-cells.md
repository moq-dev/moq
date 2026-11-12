# [S] The instruction bench rejects cells measured on a busy host

## Goal

`just bench-profile instructions` never reports a number from a cell that ran while other work loaded the host: it reruns the cell, and fails the run if the host stays busy, so every figure in its summary is comparable with the others.
Today each cell records the load it ran under (`host_load_1m` in `bench/instructions.py`) and the summary prints it, but telling a clean cell from a contaminated one is left to the reader.

## Plan

Decided 2026-11-12 (maintainer, `/quest-plan`), from an agent memory note that recorded the workaround ("compare only quiet cells within one run") instead of a fix:

- **Scope.** ✅ Fix the bench (rejected: keep the workaround as a paragraph in `bench/README.md`).
- **Milestone.** ✅ the m4 perf epic (rejected: m0, m1).

What was measured on the shared 24-CPU host, 2026-11-02 and 2026-11-03: instructions per delivered frame read 5 to 20% low once the one-minute load passed about 9, because a contended relay batches more frames per wakeup, and above about 20 cells also lost groups.
One revision read 142.7k under load and 169k to 175k quiet.
`quest gate` checks the load only when it admits the command, and the load swung from 2 to 250 during multi-hour bisects ([#7605](https://github.com/moq-dev/moq/pull/7605)).

Guidance, for the implementer to confirm:

- The threshold is a property of the host, so derive it from the CPU count or measure it rather than hard-coding 9.
- Bound the reruns, and say in the failure which cells never got a quiet window.
- The one-minute average lags a short burst. Sampling the load at both ends of the window is what the bench does now; check whether that is enough or whether the window needs its own measure, such as the run queue from `/proc/pressure/cpu`.
- `bench/local.py` records `host_load` as well; decide whether it takes the same rule.
