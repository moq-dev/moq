# [M] Retain benchmark evidence and quantify comparison noise

## Goal

`just bench BASE` produces repeatable, inspectable comparisons with an uncertainty
estimate and preserved evidence, so a small reported speedup can be evaluated.

## Plan

`rs/scripts/bench.sh` runs each relay workload once as base then current,
without repeated rounds or alternating execution order. `cleanup` deletes the run
directory, including Criterion estimates, load/host JSONL, relay logs, and
summaries. Preserve the existing default command
while extending this harness rather than creating another benchmark runner.

- Add configurable repeated paired rounds, alternate base/current order, and
  perform warmup outside the measured window. Keep the current load generator,
  workload, backend, and resolved settings identical for both revisions.
- Save individual paired results and report median paired deltas plus a documented
  dispersion/confidence estimate. Flag insufficient or noisy samples instead of
  implying precision from a single ratio. Retain Criterion's own statistics.
- Add an artifact destination retaining raw JSONL, summaries, Criterion data,
  stdout/stderr, exact revision and dirty patch, build flags, tool versions,
  hardware/kernel, allocator, affinity, workload, and execution order. Preserve
  partial evidence on failure while still cleaning up owned processes/worktrees.
- Distinguish throughput-window counters from cumulative latency/loss. Today
  `rs/scripts/bench.sh::summarize_load` differences bytes over the last five seconds
  but reads final lifetime latency and group-loss counters. Label that explicitly;
  consume windowed data when the existing latency quest supplies it. Never
  subtract percentiles or call cumulative loss a steady-state sample.
- Compare CPU per delivered byte/frame only at comparable offered load, delivery,
  and loss. Track load-generator CPU/saturation too: sharing a loopback host can
  hide a relay win behind generator limits. Record io_uring worker metrics with
  the same time bounds when available.
- Test the reducer with synthetic stable, noisy, missing, invalid, and known-delta
  samples. Validate an unchanged-revision A/A run and a deliberately degraded
  fixture; keep normal machine timing informational rather than a flaky CI gate.

## Related

- [Windowed latency](/quest/m1/3126-moq-bench-every-readme-example-fails-to-parse-and.md) - owns histogram/window semantics
- [Relay profiling](/quest/m2/performance-profiles.md) - shares workload and artifact conventions
