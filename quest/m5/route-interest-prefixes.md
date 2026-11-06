# [M] A route stream's interest is a set of prefixes

## Goal

A route-layer Announce Stream's interest is a set of prefixes instead of
one, so a subscriber that wants several disjoint subtrees opens one stream
rather than one per prefix. An ANNOUNCE_FLOOR certifies the whole set at
one revision or nothing, and the proof's floor and certification sweeps pass
over multi-prefix interests.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

Split out of [Routes and announces](/quest/m2/cluster-routing/routes/README.md) by
[moq-dev/moq#7676](https://github.com/moq-dev/moq/pull/7676), which lands
the route layer's core with a single interest prefix per stream (its
regions are suffixes of that prefix) and leaves this as a separate m2 quest.

- Define what a whole-set floor certifies (lite-07's floor is a marker over
  its one prefix, decided 2026-11-05), and how a sender that knows only some
  of the set's prefixes answers.
- Redo the proof's floor and certification sweeps with multi-prefix
  interests, including overlapping prefixes and a set that changes.
- Update the lite draft and both implementations together.

Public API: the route layer's interest type. Wire: an Announce Stream's
interest and what ANNOUNCE_FLOOR certifies on the next unpublished lite version.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the single-prefix route layer this extends
