# [M] A lite-07 claim's reach follows every route that serves it

## Goal

A Rust lite-07 answer re-judges a claim the moment what its routes may serve changes, so a
peer is never left without a claim some route can serve, or shown one none can. Today the
answer looks the claim's reach up in the origin each time it judges the claim
(`Answer::reach` in `rs/moq-net/src/lite/publisher/routed.rs`), and two changes reach it
only at the claim's next change, grant change, or edit:

- A route of the same instance at the same prefix that does not win the prefix appears,
  leaves, or changes scope: the origin's cursors report only the winning route, so the
  node tells nothing.
- A scoped neighbour's claim is told before the session that gave it serves it into the
  origin, so the lookup finds no route and the claim is shown as before the check.

## Plan

Found by the local review of [moq-dev/moq#9229](https://github.com/moq-dev/moq/pull/9229),
which carried the reach to `Answer::shows`.

- Recommended: carry the reach with the claim inside the node (on its `Attr`, never on the
  wire), so the node tells every answer when it changes and the origin lookup goes. The
  origin's own claims take it from the feeder cursor (`OwnClaims` in
  `rs/moq-net/src/model/origin.rs`), which then reports the union reach of the winner's
  instance at each prefix (`sync_route`, `TableCursor::update`); a neighbour's claims take
  it from the session that gives them (`lite/subscriber/routed.rs`, `admits`: its producer's
  scope intersected with the claim's prefix).
- Only the feeder's cursors compute the union, since every cursor touching a prefix visits
  `TableCursor::update`; bench that over routes per prefix and cursors, both swept.
- Rejected: a watch per claim the reach hides, which costs origin state per claim.
- The tests: a route of the claim's instance serving `c/public/**` announced beside a winner
  serving only `c/private/**` shows the claim to a peer granted `c/public/**` at once; a
  scoped neighbour granted `a/*/x` gives a claim at `a`, and a peer granted `a/q/y/**` is
  never shown it.

Public API: none. Wire: none.
