# [M] A route stream's interest is a set of prefixes

## Goal

A route-layer Announce Stream's interest is a set of prefixes instead of
one, so a subscriber that wants several disjoint subtrees opens one stream
rather than one per prefix. ANNOUNCE_FLOOR regions and certification hold
across the set, and the proof's floor and certification sweeps pass over
multi-prefix interests.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

Split out of [Routes and announces](/quest/m1/cluster-routing/routes/README.md) by
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676), which lands
the route layer's core with a single interest prefix per stream (its
regions are suffixes of that prefix) and leaves this as a separate m2 quest.

- Define how ANNOUNCE_FLOOR names a region under a set (which prefix it
  extends), and what a count of zero means when no prefix meets the sender's
  regions.
- Redo the proof's floor and certification sweeps with multi-prefix
  interests, including overlapping prefixes and a set that changes.
- Update the lite draft and both implementations together.

Public API: the route layer's interest type. Wire: an Announce Stream's
interest and ANNOUNCE_FLOOR regions on the next unpublished lite version.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes/README.md) - the single-prefix route layer this extends
