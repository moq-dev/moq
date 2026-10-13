# [S] Lock-wait CPU from the recorded switches

## Goal

`just bench-lock` measures each worker's CPU from the same perf recording
that measures its waits, so wait/CPU shares one clock and one window.
`/proc` schedstat only tells an idle worker from one that never switched.

## Plan

Today `bench/profile.py record` reads schedstat before `perf record` starts and
after it exits, while perf records no switch-out samples for its first ~115 ms.
CPU therefore covers a longer window than the waits do, biasing wait/CPU about
2% low on the 5 s window. A worker that ran only during perf's start-up and then
slept would be refused ("ran but never switched out"), although the recording
is valid (CodeRabbit on #5277).

Decided 2026-10-13 (maintainer AFK, recommended option taken):

- Derive CPU from the `--switch-events` records already in `perf.data`: sum
  each worker's on-CPU intervals, switch-in to the next switch-out, clipped to
  the window the samples cover. A worker's first record gives its state at the
  window start: a switch-out means it was running. Schedstat no longer supplies
  CPU.
  - Rejected: gate perf through its `--control` FIFO and take schedstat at the
    enable and disable points. It keeps two clocks and adds a control channel.
- A worker with no switch records in the window either slept or ran
  throughout, and the recording alone can't tell which (Codex on #5404). Keep
  the schedstat reads only to classify those workers: if its counter did not
  advance, the worker is idle; otherwise refuse the capture as ambiguous.
  A worker with switch records but no sampled switch-out still refuses.
- Bump `lock.json`'s `version`, since `cpu_ns` changes meaning.
- Extend `bench/profile.test.py` with a worker that runs only before the first
  sample, and a check that CPU and waits share the window. Re-run
  `just bench-lock fanout-heavy 16` and record the before/after wait/CPU in the
  PR.

## Related

- [Relay profiling](/quest/m1/perf/lock-profile.md) - the recipe this corrects
- [kio channel contention](/quest/m1/perf/kio-channel-contention.md) - reads these wait/CPU numbers
