# [M] dispatch-query's terminate test survives load

## Goal

`sh/dispatch-query.test.py`'s `test_terminating_the_run_stops_the_running_phase`
passes reliably under host load, fixed at its cause under the epic's rules,
and its `@unittest.skip` is removed. Cancellation also covers a lane launched
before registration in the PID map, so the confirmed launch race cannot leave
an uncancelled lane holding the EXIT trap open.

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
  reports a failure in 1 of 420 runs on Bash 5.3.20: the EXIT trap's
  `wait` returns a cancelled lane's 143 and errexit skips `report`. Its
  `wait || true` patch reportedly passed 420/420. This remains a lead to
  verify with a failing trace in the original environment: argument-less
  `wait` normally returns zero, while a trapped signal can interrupt it.
  Record the actual status and whether a signal interrupted the wait rather
  than assuming it propagated a child's status.
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

Decided 2026-11-05 (maintainer, /quest-plan): capture a failing EXIT-trap and
process trace under load in the original pinned Bash 5.3.20 environment next.
Investigating the older-Bash hang first was rejected because it differs from
the reported missing timing line. Keep the test skipped until its cause is
verified. This PR retains evidence only; the job-table cancellation candidate
was discarded before this head. The isolated launch race is real, but does not
establish the cause of the pinned-shell failure. After capturing that failure,
fix and verify the confirmed launch/cancellation race as part of this quest,
even if the missing-report failure has a different cause.

## Waiting

- moq-dev/moq: capture a failing EXIT-trap and process trace under load in the
  original pinned Bash 5.3.20 environment. Re-run the temporarily unskipped
  termination test beside CPU-heavy work, record the shell version, trap status
  and child/job state when the cancellation timing line is missing, and attach
  the failing trace to the investigation. Use that trace to choose and verify a
  root-cause fix before removing the skip; older-shell hangs alone do not clear
  this wait.
