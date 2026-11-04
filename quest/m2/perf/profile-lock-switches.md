# [S] Lock profiles find why switch records stop alternating

## Goal

`just bench-profile lock` (`bench/profile.py`) keeps refusing a capture
whose switch records stop alternating, and the cause of those records is
found and fixed, so 16-worker captures on a quiet host complete. Lost perf
records are reduced by a larger buffer for high-switch shapes, and the
number lost is reported beside the result.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7742](https://github.com/moq-dev/moq/pull/7742)): milestone m2,
in the perf epic. The `bench-profile lock` bullet moves here from
[cluster route memory](/quest/m2/perf/cluster-route-memory.md), since it has
nothing to do with route memory.

- Decision prompt (maintainer, 2026-11-03): cluster-route-memory said to
  tolerate and count nonalternating switch records, while #7742 said to keep
  refusing them:
  - ✅ Strict: keep refusing, find the root cause, and drop the
    tolerate-and-count bullet.
  - Tolerate and count.
- Occurrences:
  - #7742's second current-source capture at 16 workers was refused with
    `worker moq-quic-0 (1448077) has nonalternating switch records`. No
    other agent was running, so outside load is not an established cause.
  - [moq-dev/moq#7533](https://github.com/moq-dev/moq/pull/7533) could not
    record base chat at 16 workers.
  - The [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md)
    notes, and the log lock profile quest #7742 deleted, also record
    refusals.
- Check: the refusal is at `bench/profile.py:131`, with its test at
  `bench/profile.test.py:279`. Capture the raw records of a refused run and
  see whether they come from perf losing records, from records out of order
  across CPUs, or from a parse error. Fix the cause in the capture or the
  parser.
- Lost records: [moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534)
  raised the perf buffer by hand (`SPIKE_PERF_BUFFER`). Size the buffer for
  high-switch shapes, report the lost count, and cover both in
  `bench/profile.test.py`.

Public API: none. Wire: none.
