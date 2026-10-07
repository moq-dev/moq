# [S] A JS track-info query ends with its last requester

## Goal

In `@moq/net`, a pending track-info query counts as broadcast demand only
while someone still wants the answer. When the requester's TRACK stream is
reset or its session closes before the publisher answers, the query ends and
`Broadcast.Demand` drops, as in Rust.

## Plan

Found in the review of #4956. Decided 2026-10-07: mirror Rust, where the
query is a consumer of the track state that counts toward demand and drops
with the serve (`TrackInfoServe`, `rs/moq-net/src/lite/publisher.rs`).

- `resolveTrackInfo` (`js/net/src/broadcast.ts`) takes an `AbortSignal` and
  pins demand only while a requester holds the query.
- The lite publisher (`runTrackInfo` and `#resolveTrackInfo` in
  `js/net/src/lite/publisher.ts`) watches the TRACK stream for a reset and
  releases its hold; the shared per-front query ends with its last holder.
- Fold in the review nit: `removeTrack` on a name cached only by a
  `consume()` subscription is outside its documented contract; document or
  refuse it.

Test: a requester that disconnects before the answer drops broadcast
demand, and a second requester keeps it pinned until it leaves too.

Public API: `resolveTrackInfo` gains a signal argument. Wire: none.
