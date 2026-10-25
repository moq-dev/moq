# [XS] A profiling host

## Goal

Condition: a quiet Linux host has `kernel.perf_event_paranoid=-1` and
`bench/README.md` names it and how to reach it. Check with
`sysctl kernel.perf_event_paranoid` on that host. This is a maintainer action;
delete this quest once it holds.

## Plan

- `bench/profile.py` refuses `lock` and `cpu` captures without it (heap
  captures need only `jeprof`). The kio log quest and
  [moq-dev/moq#6680](https://github.com/moq-dev/moq/pull/6680) need CPU and lock
  profiles from such a host.

## Related

- [kio append-only log](/quest/m1/unbounded-groups/log.md) - needs a "before" lock profile
