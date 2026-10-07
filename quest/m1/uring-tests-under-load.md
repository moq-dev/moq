# [S] moq-uring tests pass under parallel load

## Goal

`just check` passes while several checks run on one machine. During the
2026-10-06 parallel quest run, moq-uring tests failed while concurrent runs
shared an 8 MiB `RLIMIT_MEMLOCK`, and `deadline_fires_at_park` and
`dropped_worker_rejects_operations` failed under heavy load and passed alone.

## Plan

Fix at the cause, never with a retry or longer timeout:

- Locked memory: measure what each test ring locks and whether tests can share
  or shrink rings. If configuration must change, make the supported dev-shell
  and CI setup actually allow the tests to pass under parallel load.
  `Error::ring` already reports `RLIMIT_MEMLOCK` on ENOMEM; check that this
  reaches the failing tests. A clearer failure alone does not meet the goal.
- The worker tests: check the two named tests for wall-clock dependencies and
  move them to mocked time or event assertions. `remote_wake_unparks` belongs
  to [Papercuts](/quest/m1/papercuts.md), not this quest.
- Keep this resource-sharing follow-up standalone: it has no shared fixture
  with the current children of the load-flake questline. Run the affected
  tests during parallel checks and wire any new coverage into CI.

## Related

- [Papercuts](/quest/m1/papercuts.md) - owns the remote worker wake assertion
- [More tests under load](/quest/m1/test-flakes-2/README.md) - the same cause-first rules and final loaded check apply here
