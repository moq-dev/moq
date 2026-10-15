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

Decided 2026-10-13 (maintainer): m0. Groups park in production during a
same-epoch relay handover: `Copy::wire` (`rs/moq-net/src/model/resume.rs`)
caps the replaced copy's range, the lite publisher treats that end group as
a serving cap, and the new route's unbounded demand then drains the parked
set quadratically. Live players setting no cap of their own doesn't help.

Recommended: validate lazily. Wake the subscriber from the track (one
registration) when a cached group aborts, rather than registering on every
parked group per poll, and on that wake remove exactly the parked entries
that aborted, cap raised or not. Aborted entries are not a low-end prefix:
eviction rotates recently read groups to the back and expiry follows last
access. A sequence can name both a live group and a fetched head, so confirm
the parked consumer itself aborted before dropping it, and keep any abort
bookkeeping bounded by what the cache retains, not by lifetime churn. Each removal is paid for by its insertion, and
the `BTreeMap`'s O(log P) removal is accepted rather than swapping the
structure. Keep the existing guarantees: a raised `start_at` drops overtaken
entries, eviction bounds the parked set to what the cache retains, a
finished track with only aborted parked groups ends, and an abort between
check and registration still wakes the reader. A track-wide wake fans out to
every capped subscriber, so extend the benchmark to sweep parked count
against capped subscribers per track: the parked axis flattens, and a
per-abort cost that grows with subscribers shows up as a slope.

Public API: none. Wire: none.
