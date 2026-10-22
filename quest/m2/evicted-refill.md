# [M] Refill groups evicted before a reader took them

## Goal

A reader behind the live edge still gets a group the cache evicted after the
reader judged it but before it took it: the group becomes a miss and is asked
of the track's fills, like one evicted before the reader judged it. A reader
never receives a group it already took twice, and slow readers under cache
pressure cannot make the relay refetch without bound.

## Plan

From [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171), where the
maintainer chose to land fills without this (2026-10-21). Today a reader
judges each sequence once: a group cached when judged and evicted later is
skipped silently.

- Track evictions per reader. An ordered reader can judge again every
  eviction at or above its next sequence. An arrival-order reader delivers
  above its floor, so it must tell the groups it already walked past from
  the ones still ahead, for example by keeping each evicted group's arrival
  index. Bound what the track keeps by the slowest reader, not by the cache.
- Guard against thrash. Under pool pressure a refill evicts another group,
  possibly the next one the same slow reader needs. Decide in the PR, with
  options and a recommendation, how a reader stops refilling once its
  refills only evict what it reads next: give up to the drop cursor, or rely
  on its max delay.
- Test a slow ascending reader on a pooled track under bufferbloat: it
  receives every group its budget covers, takes none twice, and refills stay
  proportional to what it reads.

Public API: none. Wire: none.

## Related

- [Unbounded groups](/quest/m2/unbounded-groups/README.md) - refills the parts of a group the cache sheds under pressure
