# [M] A track's pending tail counts arrivals, not its cache

## Goal

A received track decides which groups are still missing below a declared end
from a per-track record of live groups that arrived, which eviction never
touches. An end check stops scanning the cache, and a reader whose track
ends short because a never-arrived group was aborted gets the abort error
instead of a clean end. A reader that got every group still ends cleanly.

## Plan

PR #4225 judges holes from the cached range (`finish_at_pending` /
`set_tail_pending` in `rs/moq-net/src/model/track.rs`), so each end check is
O(groups), and an evicted group, a datagram, or an abort over a group that
never arrived all look alike. Decided 2026-10-10: record live arrivals per
track (fetched copies never count, as in #4225), bounded by the subscription's
grace like `tail::Tail`, and error the reader on an aborted hole.
Open: whether an arrival that ages past grace before the end is declared still
counts as arrived or becomes a hole.

Add a benchmark swept over tracks and groups held at the tail, per AGENTS.md's
fan-out rule. Mirror the record in JS through
[JS pending tail](/quest/m1/js-pending-tail.md), which waits for this.

Public API: behavior only (a short tail errors). Wire: none.
