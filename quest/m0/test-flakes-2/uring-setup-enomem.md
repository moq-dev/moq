# [S] io_uring ring setup survives a fragmented host

## Goal

moq-relay's io_uring tests, such as `bitrate_caps::uring_session_is_held_to_its_caps`,
stop failing at `moq_uring::Worker::new` with `ENOMEM` on a loaded Linux 7.3
host, and the relay's startup error names the cause the kernel actually hit.

## Plan

Seen 2026-11-13 on fedora (Linux 7.3.3, `ulimit -l` 8192) during a loaded
`just check`: the test's capability probe got `Cannot allocate memory (os error
12)` from `io_uring_setup` within 10 ms, and the rerun passed.

It was not `RLIMIT_MEMLOCK`, despite the message blaming it. Since
[moq-dev/moq#8641](https://github.com/moq-dev/moq/pull/8641) showed 7.3.3 sets
up a ring under a limit of 0, and v7.3's `io_uring.c` creates both ring regions
with `io_create_region(NULL, ...)`, so nothing is charged to the user. The
host's memory cgroups recorded no `max` or `oom` events either.

The likely cause, from v7.3's `io_uring/memmap.c` `io_region_allocate_pages`:
it first tries one compound allocation of `get_order(size)`, and on failure
falls back to `alloc_pages_bulk_node`, returning `ENOMEM` whenever the bulk
call hands back fewer pages than asked. The bulk allocator returns a partial
batch rather than reclaiming. The production CQ of 2048 entries is 32 KiB of
CQEs plus the ring header and SQ array, so the region spills just past 32 KiB
into an order-4 (64 KiB) compound allocation, above `PAGE_ALLOC_COSTLY_ORDER`,
which the page allocator gives up on quickly when memory is fragmented.

Open (implementer): confirm the cause, for example by fragmenting memory or
tracing `io_region_allocate_pages`'s returns while the test loops, before
changing code. Candidate fixes, in order of preference:

- Size the rings so each region stays at or below order 3 (32 KiB). The
  rings region holds the header and SQ array besides the CQEs, and 2048 CQEs
  already fill 32 KiB, so `IORING_SETUP_NO_SQARRAY` alone cannot do it: the CQ
  must shrink too, such as to 1024 entries. This touches the CQ sizing
  rationale in `rs/moq-uring/src/worker.rs`.
- Report it upstream if the bulk fallback's partial return is the whole cause.

Never retry setup in the test. `Error::ring`'s message should stop naming
`RLIMIT_MEMLOCK` first on kernels that do not charge it.
