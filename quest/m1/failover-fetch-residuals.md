# [M] A fetch survives the track ending on a relay chain

## Goal

A relay whose track ended still moves a fetch off a route that dies:
`residual_a_fetch_cannot_move_off_a_dying_route_after_the_end` in
`rs/moq-net/tests/integration/failover_chain.rs` flips to assert delivery, and
`EndedRelay.cfg`'s open finding is gone (its `Mesh` merged into
`FailoverChain.cfg` if the bounds allow).

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
The first two residuals (a fetch in flight when the track ends, and a fetch
asked of a completed copy) are fixed: a relay's finished copy keeps serving
fetches while it is read.

What remains is the mesh case. B's front ends the track (`Action::Finish`),
so its logical track closes and reads as unread; when A dies, nothing splices
a copy from B's direct route to O, and the fetch's `Recover` waits on a
concluded route until the subscriber's budget runs out.

- Keep an ended track routable for a fetch in flight in `model/front.rs`,
  minding the parked index and `forget`.
- In `FailoverChain.tla`, allow `Switch` for a track with `end = "ok"` whose
  serving copy died, and adjust the `Abort` guard to match.
- Run `/verify-failover` and `/verify-resume` on the head.

Public API: none. Wire: none.
