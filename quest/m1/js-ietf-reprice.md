# [S] JS IETF reprices a namespace in place

## Goal

When a route's price changes but its original publisher does not, `@moq/net`'s IETF publisher reprices the namespace the peer already holds in place, as Rust does, instead of withdrawing it and advertising it again. A subscriber never sees a namespace briefly vanish because its cost or hop chain moved, unless the peer refuses or ignores the update.

## Plan

Rust already does this in `rs/moq-net/src/ietf/publisher.rs` (`sync_namespace`, `update_namespace`). `js/net/src/ietf/publisher.ts` withdraws first on both paths, so that "a republish or re-price reads as withdraw-then-advertise". Match Rust:

- PUBLISH_NAMESPACE: send REQUEST_UPDATE on the request that carries the namespace, with only the changed parameters, and wait for its answer so one update is outstanding per stream (which satisfies MAX_REQUEST_UPDATES). A REQUEST_ERROR withdraws it and an unanswered update drops the request; either way the retry re-offers it fresh.
- A different original publisher (first hop) or a different broadcast still withdraws the PUBLISH_NAMESPACE and advertises again.
- Inline (a solicited SUBSCRIBE_NAMESPACE, draft-16+): re-send NAMESPACE on the response stream for any change, first hop included, without NAMESPACE_DONE first. The receiver treats it as a replacement, and its origin already ends a broadcast whose first hop changes.

The wire-visible comparison from the announce-dedupe work decides whether anything is sent at all.

Found while landing the announce update dedupe (#4423). No wire change: REQUEST_UPDATE is already in the IETF drafts the tree negotiates.

Tests: a price change on a held namespace sends one REQUEST_UPDATE and no PUBLISH_NAMESPACE_DONE; a nonzero cost repriced to 0 carries an explicit cost 0, since REQUEST_UPDATE keeps an omitted parameter; a hop path change behind the same first hop sends one REQUEST_UPDATE carrying HOP_PATH and no withdrawal; a solicited namespace is repriced with one NAMESPACE and no NAMESPACE_DONE; a first-hop change and a republish still withdraw a PUBLISH_NAMESPACE; a REQUEST_ERROR withdraws; with mocked time, a second change while an update is unanswered sends nothing until it resolves, and an update that times out drops the request and re-offers the namespace on a fresh one.
