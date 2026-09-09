# [S] moq-bench: per-interval latency percentiles

## Goal

A steady-state window of a `moq-bench` run has its own latency distribution:
the `--startup` ramp no longer bakes into `latency_p50_ms`, `p90`, `p99`, and
`max`, so two JSONL lines can be joined over a window the way the counters
already can.

## Plan

`Latency` (`rs/moq-bench/src/stats.rs:208-253`) accumulates buckets for the
whole run and snapshots cumulative percentiles. The README documents them as
cumulative (`rs/moq-bench/README.md:47-48`) and tells the reader to skip the
ramp (`:152`), which the counters allow and latency does not: p99 reads as
pure ramp artifact (hundreds of ms against a 1-2 ms steady p50) because a few
first groups landed while the swarm was still connecting.

- Emit per-interval percentiles (or the interval histogram) beside the
  cumulative ones: keep the previous snapshot's buckets, diff, and compute
  p50/p90/p99 over the delta. `latency_samples` is already per-line.
- Document the new fields and update the methodology paragraph.

The README examples parse today (`--connect`, `README.md:66-71`; `--file` is
a real flag, `rs/moq-bench/src/config.rs:95-98`; the released
`--client-connect` spelling is refused by name, `config.rs:389-395`).

moq-bench is 0.0.x, so this lands on main.

## Closes

- [#3126](https://github.com/moq-dev/moq/issues/3126) - close this issue when the quest finishes
