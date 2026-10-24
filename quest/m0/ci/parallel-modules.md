# [M] Run the queue's modules in parallel

## Goal

`just ci` runs its selected modules concurrently instead of one after another,
with the same modules selected and the same failure behavior, and a
multi-language entry finishes measurably faster.

## Plan

Decided with the maintainer on 2026-10-23. Measured 2026-10-23: `sh/dispatch.sh`
runs every selected module serially. On multi-language entries (863 to 876 s),
the cpp, dart, py, kt, and obs modules take about 400 to 500 s that could
overlap the Rust build, an estimated 45% off those entries.

- Bound concurrency so modules do not starve each other, and keep each
  module's output readable when they interleave.
- A failure still fails the run and names its module.
- Locally, `sh/local.sh` admits each expensive phase through `quest gate`
  and defaults nextest to four workers; overlapping modules keep that bound.

Public API: none. Wire: none.
