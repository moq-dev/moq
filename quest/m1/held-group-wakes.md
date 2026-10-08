# [S] A group held across a route switch wakes when it goes stale

## Goal

A group held across a route switch is woken when it goes stale: its
successor's first timestamp, its successor's abort, or a newer group's first
timestamp moving the edge. Today it waits for the next unrelated append.

## Plan

Split from the expiry-wakes quest, which landed the deadline index
(`model::expiry::Wakes`, owned by `cache::Track`) for `GroupServe` reads.

`Recover::poll` in `rs/moq-net/src/model/resume.rs` gives up a held group on
an old route through `track::Consumer::poll_stale`, which registers only on
the track state. Register the held group in the same `Wakes` index instead. One
mechanism, not a per-reader helper beside the index. Two differences the
index must carry: an entry can wait on its successor's first stamp with no
deadline yet, and Recover's entry lives in the serving copy's index (a
different track from the one holding the group), so it moves when the route
changes.

Decided 2026-10-08: this is the one wake mechanism for held groups, and it
moves to m1 beside [One max_age meaning](/quest/m1/cache-max-age.md). That
quest's wall-clock budget (successor arrival plus budget) becomes a deadline
on the same `Wakes` entry rather than a second per-reader timer. Whichever
lands second adds its trigger to the entry.

Tests: add #4950's `a_group_no_route_continues_wakes_when_its_successor_is_stamped`
to `resume.rs` (it would fail today), and extend it to the successor aborting
and a newer group stamping the edge.

Public API: none. Wire: none.

## Closes

- [#4950](https://github.com/moq-dev/moq/issues/4950) - a group held across a route switch is not woken when its successor gets its first timestamp

## Related

- [One max_age meaning](/quest/m1/cache-max-age.md) - arms its wall-clock deadline on this entry
