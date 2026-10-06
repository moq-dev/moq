# [S] moq-uring tests pass under parallel load

## Goal

`just check` passes while several checks run on one machine. Today moq-uring
tests fail when concurrent runs exhaust the user's shared `RLIMIT_MEMLOCK`
(8 MiB by default), and `deadline_fires_at_park`,
`dropped_worker_rejects_operations`, and `remote_wake_unparks` have failed under
heavy load and passed alone.

## Plan

Seen repeatedly during the 2026-10-06 parallel quest run. Fix at the cause,
never with a retry or longer timeout:

- Locked memory: find what each test ring locks and whether tests can share
  or shrink rings; failing that, decide whether the dev shell should raise the
  limit, or the tests should detect the limit and fail with a clear message.
- The worker tests: check them for wall-clock dependencies and move them to
  mocked time or event assertions.
