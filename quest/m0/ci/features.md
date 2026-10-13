# [M] PR builds compile only what they check

## Goal

A PR's feature checks cover the feature sets its diff can affect, and its test
compile builds only the targets it runs. Nightly keeps the exhaustive
feature coverage.

## Plan

A transitive change triggers many isolated feature builds, and ordinary test
runs compile benchmarks they never execute. Measure which feature builds and
bench compiles a typical PR pays for before moving any of them, and record
where the moved coverage now runs.
