# [S] Draining parked groups is near-linear in the backlog

## Goal

Once a subscriber's raised cap releases its parked groups, each delivery
costs at most O(log P), so draining P parked groups is O(P log P), not
quadratic. The `origin/resume_overlap` benchmark's per-group drain time no
longer grows linearly with the parked count.

## Plan

Found on [#5446](https://github.com/moq-dev/moq/pull/5446): 1024 parked groups
drain at about a tenth of the rate of 64, on `main` as well.
`track::Subscriber::poll_recv_group` (`rs/moq-net/src/model/track.rs`) runs
`parked.retain(..)` over every parked group on every poll, re-registering an
abort waiter on each, so a drain is quadratic.

Decided 2026-10-13 (maintainer): m1, not m0. Groups park only when a
subscriber caps its range below arriving groups and later raises the cap;
live players and default relay forwarding set no end cap, so production's
parked set is empty today.

Recommended: validate lazily. On every poll, cap raised or not, pop aborted
groups off the low end of `parked` until the lowest is live, and wake the
subscriber from the track (one registration) when a cached group is evicted
or expires, rather than registering on every parked group per poll. Each pop
is paid for by its insertion, and the `BTreeMap`'s O(log P) removal is
accepted rather than swapping the structure. Keep the existing
guarantees: a raised `start_at` drops overtaken entries, eviction bounds the
parked set to what the cache retains, a finished track with only aborted
parked groups ends, and an abort between check and registration still wakes
the reader. Extend the benchmark with a parked-count sweep that shows the fix
flattens it.

Public API: none. Wire: none.

## Required

- [Resume duplicates](/quest/m0/broadcast-epoch/resume-duplicates.md) - reshapes `track::Subscriber`'s delivery record in the same code
