# [XS] dispatch-query's terminate test survives load

## Goal

`sh/dispatch-query.test.py`'s `test_terminating_the_run_stops_the_running_phase`
passes reliably under host load, fixed at its cause under the epic's rules,
and its `@unittest.skip` is removed.

## Plan

Planned 2026-11-04 (maintainer) from the stream-credit scheduler
([moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)), whose
`just check` saw it fail once on a loaded host while it passed 3/3 alone;
that PR does not touch `sh/`. It is skipped meanwhile, per AGENTS.md's rule
to disable a flaky test at once. Reproduce it under load (repeated runs beside
a CPU-heavy job), find the deadline or ordering it races on, and assert on
the event instead of a wait, as the epic's rules say.

Investigation 2026-11-05:

- [pullfrog's candidate fix](https://github.com/moq-dev/moq/commit/e6690144ca14f9ec8c48a56b4ed26c5a65a47708),
  removed from [moq-dev/moq#7789](https://github.com/moq-dev/moq/pull/7789),
  changes the EXIT trap's bare `wait` to `wait || true`, claiming a cancelled
  lane's status skips reporting under errexit. This remains unverified.
- With the skip removed temporarily, the existing test passed alone and in
  100 loaded runs on the pinned Nix Bash 5.3.20. The loaded harness restricted
  itself and four CPU workers to two CPUs; each run covers CHECK_JOBS=1 and 3.
- The same harness using host Bash 5.3.9 for fixture shells had two
  `communicate(timeout=60)` errors in 100 runs, both CHECK_JOBS=3. This is a
  hang rather than the originally reported missing cancellation timing line.
- A candidate that cancelled Bash's job table rather than the `running` map
  still had one older-shell hang in 100 runs. It is insufficient; no dispatcher
  change is retained, and the test stays skipped pending a verified fix.
- A scratch launch barrier can hold the parent before registering a Rust lane.
  The original PID-map cancellation hung for the existing 60-second test
  deadline in that forced window; job-table cancellation passed in 0.072 s.
  This isolates a launch race, but does not explain the residual older-shell
  hang or prove the original missing-report failure's cause.
- A gated full `just ci origin/main` with the temporary candidate completed:
  every lane except flake and CARLA passed, including the unskipped tooling
  test. Flake failed on `/proc/self/uid_map` permission; CARLA's
  `sim/python.test.sh` saw inherited Nix PYTHONPATH. Both are also reported in
  [moq-dev/moq#7710](https://github.com/moq-dev/moq/pull/7710); they were not
  repaired here. These results do not validate a final implementation.

Next: capture the EXIT trap and child process state at an actual missing-report
failure, and distinguish that failure from the older-shell hang before choosing
between trap-status handling and launch/cancellation changes. Preserve the
existing assertions and remove the skip only with a verified cause.
