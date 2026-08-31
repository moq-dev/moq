# [M] moqsrc: a rendition nobody answers keeps the session alive after the catalog closes

## Goal

Implement and verify the behavior tracked in [#3139](https://github.com/moq-dev/moq/issues/3139)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Split out of [#3130](https://github.com/moq-dev/moq/pull/3130), where two attempts at this traded one bug for a worse one.

`moqsrc`'s pumps each wait for their subscription to resolve. If a catalog names a rendition the publisher never serves, that pump waits forever. When the catalog track then closes, the session loop has nothing left to make progress on: it waits for the remaining pumps to drain, and that one never does. The session holds its connection open and never delivers a terminal EOS until the broadcast ends or the element is stopped.

Cancelling the not-yet-live pumps on catalog close is the obvious fix and it is wrong. "Has not taken a pad yet" does not mean "will never take one" -- it also describes a pump that was spawned microseconds ago and has not been polled. A publisher that names its tracks and then finishes the catalog produces exactly that: the snapshot and the close arrive back to back, so the close is routinely observed before the freshly spawned pumps run at all, and every rendition it just announced gets cancelled before it can subscribe. That drops the entire broadcast instead of one bad rendition, which is far worse than the hang. #3130 tried it, reproduced the media loss in a test, and reverted; `a_closing_catalog_keeps_the_renditions_it_named` guards against re-introducing it.

Separating the two needs a signal the catalog does not carry today. Some options, none obviously right:

- A bounded grace period after the catalog closes, before giving up on a still-unresolved subscription. Simple, but it is a timeout standing in for information, and picking the number is guesswork.
- Ask the moq layer whether a track can still be served: a subscription with no producer and no `Dynamic` that could ever answer it is already knowable inside `broadcast::Consumer`, and surfacing it would make this decidable rather than timed.
- Treat catalog closure as a promise that the announced set is final, and have the publisher side finish tracks it never intends to serve, so the subscription resolves rather than parking.

The middle option looks the most principled, since it fixes the class rather than this element, but it widens `moq-net`'s surface.

Note this is not a regression from #3130: before it, the same input parked `reconcile` inside the catalog loop, so the session hung there instead.

## Closes

- [#3139](https://github.com/moq-dev/moq/issues/3139) - close this issue when the quest finishes
