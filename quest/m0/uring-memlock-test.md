# [S] The io_uring memlock startup test holds on current kernels

## Goal

`moq-relay`'s `runtime_uring::uring_startup_reports_the_memlock_limit`
passes on every host the gate runs on, or is rewritten or deleted so it
pins what the relay actually guarantees.

## Plan

Found 2026-11-12 by a `just check` on a Fedora host running Linux 7.3,
unrelated to the change under test: the test sets `RLIMIT_MEMLOCK` to 0
and expects `ready.wait().await.expect_err("a worker needs locked memory")`
(`rs/moq-relay/tests/integration/runtime_uring.rs`), but the io_uring worker
starts anyway. One re-run with `--features io-uring` failed the same way, so
it is deterministic on that host, not a flake. A likely cause, unconfirmed:
newer kernels charge io_uring rings to the memory cgroup rather than
`RLIMIT_MEMLOCK`, so a ring may no longer need locked memory at all. Find which ring setup still charges
the limit, if any, and decide from that: keep the error path and make the
test provoke it in a way the kernel enforces, or drop the expectation if the
relay no longer needs locked memory.

## Hosts

- linux - io_uring
