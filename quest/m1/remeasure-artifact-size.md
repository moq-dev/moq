# [S] Release remeasure uploads only its evidence

## Goal

Release remeasure's artifact keeps the report, versions, and per-cell logs that explain a planned break, not build output, so each run stores megabytes instead of gigabytes.

## Plan

[Run 44292060870](https://github.com/moq-dev/moq/actions/runs/44292060870) uploaded a 10 GB `release-remeasure` artifact (3256 files) because `.github/workflows/release-remeasure.yml` uploads all of `HARNESS_RUNS`, which `HARNESS_KEEP=1` fills with staged binaries, released binaries, and the released JS install from `test/interop/compat.sh`.
Narrow the upload paths to the logs and reports (or stage binaries outside the kept run directory), then confirm on a dispatched dry run that the artifact still holds every failing cell's logs.
