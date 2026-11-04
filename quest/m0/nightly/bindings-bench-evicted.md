# [S] The binding benchmark's 64 KiB case measures

## Goal

Nightly's `Binding benchmark runners` job passes: the 64 KiB case of the
moq-ffi throughput benchmark (`rs/moq-ffi/src/bench.rs`, run by each
binding's runner such as `py/moq-rs/benches/ffi.py`) completes and reports
a number instead of failing `evicted` on `main`.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [Nightly passes on main](/quest/m0/nightly/README.md)
- m1
- m2

- Confirm the cause first. The likely one: each case's origin cache is
  `CACHE_BYTES` (64 MiB), and a 64 KiB case writes 64-frame groups
  (`frames()` caps at 64), so a round can outgrow the cache before a slow
  binding reads it.
- Then size the case or its cache so it measures throughput rather than
  eviction, and keep the other sizes' numbers comparable with past nights.
- Run every binding's runner locally (`just gh nightly bindings`) before
  pushing.

Public API: none. Wire: none.
