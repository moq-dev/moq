# [M] Subscription updates reach queued and taken fills

## Goal

When a reader changes its priority, delay, or order, fills already queued or
taken by a request handler serve under the new values.

## Plan

After [Dynamic range requests](https://github.com/moq-dev/moq/pull/6171), a
`track::Fill` keeps the priority, delay, and order its joins gave it. Once a
handler takes it, an update cannot reach it, and a queued one is not
refreshed. A reader that switches to descending still has filled groups
served in the old order. Cover queued and taken fills with one mechanism,
and test an order switch mid-fill.

Public API: possibly a handler-facing update. Wire: none.
