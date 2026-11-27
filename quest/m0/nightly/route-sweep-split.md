# [S] Nightly's route sweep and route benchmarks fit their job timeouts

## Goal

Nightly's `Tests (test route-sweep)` finishes inside its 60-minute timeout, and the route
benchmarks it used to carry (bakeoff, warmth, node layer) run to completion in a matrix entry
of their own, also under 60 minutes, on a scheduled run on `main`.

## Plan

`test/route-sweep.sh` has been cancelled at the job timeout every night since
[moq-dev/moq#9200](https://github.com/moq-dev/moq/pull/9200) restored its benchmark steps
(runs [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) through
[45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463)). It is not hung:
run 45667362304 spent about 17 minutes building the integration tests, 80 s on the mesh
sweep, 8 minutes building the lib tests, 1124 s on `bakeoff::benchmark_bakeoff_growth`
(900 to 1535 s across nights), 38 s on the multicast steps, and was cancelled 14 minutes into
`warmth::benchmark_warmth_growth`. `node_layer` never ran.

Decided 2026-11-26 (maintainer, /quest-plan): ✅ split the job (rejected: raise its timeout to
120 minutes; shrink the growth sweeps). Keep the sweep steps (mesh sweep, multicast) in
`test route-sweep` and move the `benchmark_` steps to a new recipe, such as
`test route-bench`, as its own entry in Nightly's `tests` matrix. Measure the new entry's
time from a scheduled run; if it still doesn't fit, split again rather than raise the timeout.
Update the matrix comments in `.github/workflows/nightly.yml`.

Public API: none. Wire: none.
