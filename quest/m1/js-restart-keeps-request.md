# [S] A resolved JS request survives a Restart, as in Rust

## Goal

When a dynamic claim's epoch changes (a `Restart` downstream), an
already-resolved `@moq/net` request keeps its resolved broadcast, as Rust's
does, instead of ending as unroutable. Subscriptions on the old broadcast stay
until the application drops them, per the broadcast-epoch line; new requests
resolve under the new epoch.

## Plan

Found in #5141 (2026-10-10): on an epoch change JS ends a resolved request
handle (it reports unroutable), while Rust keeps the resolved broadcast. Open
track subscriptions survive in both. Decided 2026-10-10: align JS to Rust,
since the broadcast-epoch README keeps subscriptions on the old broadcast
until the application drops them. Rejected: ending resolved requests in both,
and documenting the difference.

Find where `js/net/src/origin.ts` ends the resolved handle on a server reset
and keep it, without letting it serve anything new under the old epoch
(never-stitch still holds). Mirror Rust's
`a_newer_epoch_leaves_the_broadcast_in_flight` (`rs/moq-net/src/model/origin.rs`)
in `js/net/src/origin.test.ts`, failing before the fix.

Public API: behavior only. Wire: none.
