# [M] Subscription updates reach queued and taken fills

## Goal

When a reader changes its priority, delay, or order, fills already queued or
taken by a request handler serve under the new values. A reader that joins a
taken fill with an earlier start frame gets those frames instead of
`NotFound`.

## Plan

After [Dynamic range requests](https://github.com/moq-dev/moq/pull/6171), a
`track::Fill` keeps the priority, delay, and order its joins gave it. Once a
handler takes it, an update cannot reach it, and a queued one is not
refreshed. A reader that switches to descending still has filled groups
served in the old order. Cover queued and taken fills with one mechanism,
and test an order switch mid-fill.

The start frame has the same gap (found 2026-10-24, from the code, not yet
reproduced): `FillState::join` (`model/track/fill.rs`) widens the first
group's start frame only while the fill is queued. A reader asking for an
earlier `frame_start` than a taken fill joins it unwidened, and after accept
`Fetching::poll` (`model/track.rs`) finds the cache short of that start and
returns `NotFound` without re-queuing. Reproduce it first, then fix it with
the same mechanism, so a late earlier join gets its frames.

Public API: possibly a handler-facing update. Wire: none.
