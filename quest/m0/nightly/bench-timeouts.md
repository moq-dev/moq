# [S] Nightly's benchmark jobs finish within their hour

## Goal

Nightly's "Rust bench targets" and "JS benchmarks" jobs finish inside their
timeout and publish their series again, and the Nightly triage counts a
cancelled job as a failure, so a job that times out every night is owned.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
"Rust bench targets" was cancelled 10 of the 14 nights before 2026-11-14
(every night since 11-09), and "JS benchmarks" 11 of 14 (every night since
11-04); in run 43601796176 the Rust job was still compiling the paired base
in `/tmp/moq-bench-base.*` when `timeout-minutes: 60`
(`.github/workflows/nightly.yml`) cancelled it. A cancelled job is not a
failed one, so the failure-based triage never named either; about 2
job-hours a night produce nothing. Find why the paired base builds cold
though the previous Nightly built that same commit
([moq-dev/moq#8763](https://github.com/moq-dev/moq/pull/8763) notes cache
units that were "not looked up"), fix that at the cause, and only then split
or trim the jobs if they still overrun. Never raise the timeout to fit.

Public API: none. Wire: none.
