# [S] JS IETF reprices a namespace in place

## Goal

When a route's price changes, `@moq/net`'s IETF publisher reprices the namespace the peer already holds with REQUEST_UPDATE on the request that carries it, as Rust does, instead of withdrawing it and advertising it again. A subscriber never sees a namespace briefly vanish because its cost or hop chain moved.

## Plan

Rust already does this: `rs/moq-net/src/ietf/publisher.rs` reprices with REQUEST_UPDATE and respects MAX_REQUEST_UPDATES. `js/net/src/ietf/publisher.ts` withdraws first, so that "a republish or re-price reads as withdraw-then-advertise". Keep withdraw-then-advertise only for a republish, meaning a different broadcast. For the same broadcast at a new warm cost or MoQ Cluster hop path, send REQUEST_UPDATE. The wire-visible comparison from the announce-dedupe work decides whether anything is sent at all.

Found while landing the announce update dedupe (#4423). No wire change: REQUEST_UPDATE is already in the IETF drafts the tree negotiates.

Tests: a price change on a held namespace sends one REQUEST_UPDATE and no PUBLISH_NAMESPACE_DONE; a republish still withdraws; a peer's MAX_REQUEST_UPDATES limit is honored the way Rust honors it.
