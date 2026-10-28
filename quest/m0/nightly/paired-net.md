# [M] Resolve the paired network microbenchmark regressions

## Goal

The reported network microbenchmark regressions are attributed and genuine extra costs are fixed with paired measurements.

## Plan

[Rust bench targets](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001853) compares `270395a337`
against `22808bbb2f` and flags fourteen cases: lite codec decode +31.2%, encode
+67.7%; group relaying +23.2%; batched group writes +16.6% to +45.3%; origin
route churn +14.4% and idle serving +46.2%; and four track-budget widening
cases +12.9% to +24.5%. The `bench-rust` artifact retains paired samples.

Attribute the deltas using same-host comparisons, separating production work
from changed benchmark workloads and runner/allocator noise. The codec's
borrowed-message change landed between these revisions; inspect it without
assuming it explains every case. Fix genuine regressions and retain a swept
benchmark for each affected scaling dimension. Threshold calibration remains
in [bench CI](/quest/m1/bench-ci.md), not a reason to dismiss these deltas.

Public API: none expected. Wire: none expected.
