# [S] The relay's io_uring memlock test holds on every kernel

## Goal

`moq-relay`'s `runtime_uring::uring_startup_reports_the_memlock_limit` passes
on every Linux host `just check` runs on, or is skipped where the kernel never
charges a ring to `RLIMIT_MEMLOCK`. The relay keeps reporting the memlock hint
where the limit does refuse a worker.

## Plan

Found 2026-11-12 while checking
[moq-dev/moq#8546](https://github.com/moq-dev/moq/pull/8546). On a Linux
7.3.3 host, `cargo nextest run -p moq-relay --all-features -E
'test(uring_startup_reports_the_memlock_limit)'` fails the same way on
`origin/main` and on that PR's branch: `a worker needs locked memory: ()`
(`rs/moq-relay/tests/integration/runtime_uring.rs:1328`). The test sets
`RLIMIT_MEMLOCK` to 0 and expects the first io_uring worker to fail. On this
kernel the ring starts anyway, so the relay never refuses.

Newer kernels charge io_uring memory to the memory cgroup rather than to
`RLIMIT_MEMLOCK`. Find which ring setups still hit the limit, then either
make the test force a setup that does, or detect at startup whether the limit
applies and skip the test where it cannot fail. Never loosen the assertion on
the message the relay prints. If no kernel the relay supports still charges
the limit, the startup hint and this test may go.

## Hosts

- linux - io_uring
