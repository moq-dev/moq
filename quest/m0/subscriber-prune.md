# [XS] A track drops departed subscribers

## Goal

A track's subscription list holds only live subscribers. Today
`register_subscription` (`model/track.rs`) pushes without pruning, and closed
entries are only removed when the aggregate changes. While at least one
viewer stays, churning viewers with identical preferences accumulate, and
`combined_subscription` walks all of them on every wake.

## Plan

- Remove an entry when its subscriber drops, or `retain` live entries on push
  and on each poll, whichever keeps the wake path cheapest.
- A unit test that churns identical subscribers under one steady viewer and
  asserts the list length stays bounded.
- A benchmark swept over steady viewers and churn rate.

Public API: none. Wire: none.
