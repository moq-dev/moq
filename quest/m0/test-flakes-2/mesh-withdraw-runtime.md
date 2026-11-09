# [S] moq-net's random-mesh withdrawal tests fit the gate

## Goal

`moq-net`'s `mesh_withdraw::random_meshes_retract_once` and
`capped_random_meshes_retract_once` run in a few seconds on a loaded host,
well under nextest's 60 s kill (`.config/nextest.toml`, whose stated ceiling
for an honest test is about 9 s), and their `#[ignore]` and
`sh/gh/test-inventory.allow` entries are removed. They still check every one
of the 200 seeded meshes retracts exactly once.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete): ✅ quarantine now and plan
this quest (rejected: leave them in the gate and only plan; move them to
nightly without a quest). Reported by
[moq-dev/moq#8247](https://github.com/moq-dev/moq/pull/8247)'s iterate, whose
`just check` saw both hit the 60 s kill. On `main` they take about 40 s and
45 s even alone, so any load pushes them over. CI passed them, so this is cost,
not a race.

Find where the time goes before changing anything: the 200 seeds in
`withdrawal_counts` each build a mesh, publish, close, and wait out the
withdrawal, so a wall-clock wait per seed (such as the 300 ms hold) costs
minutes in total. Drive the waits on the test clock (`#[kio::test]`'s
manual clock), or run the seeds concurrently, so the work is CPU-bound and
small. Cutting seeds is the last resort: the counts in each test's doc
comment came from all 200. If neither fits, move them to the nightly suite
with a caller instead of ignoring them. Never raise the timeout.

[Route layer acceptance](/quest/m2/cluster-routing/routes/acceptance.md)
owns the `residual_*` counterexamples in the same file; keep those as they
are.

Public API: none. Wire: none.
