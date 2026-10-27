# [S] moq-uring allocation test under load

## Goal

`steady_state_sends_do_not_allocate` (`rs/moq-uring/tests/allocations.rs`)
and the other moq-uring tests pass under a parallel `just check`.

## Plan

- Seen as 1001 allocations over 1000 sends in
  [moq-dev/moq#6496](https://github.com/moq-dev/moq/pull/6496). Find the extra
  acquire (likely a second pending acquire's waiter under load) and count what
  the test means to bound, without loosening it.
- moq-uring tests also hit the shared `RLIMIT_MEMLOCK` under load
  ([moq-dev/moq#6538](https://github.com/moq-dev/moq/pull/6538)); serialize
  them with a nextest test group if that is the cause.
