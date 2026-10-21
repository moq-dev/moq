# [S] qmux refuses over-limit creates without waiting

## Goal

`@moq/qmux` (the WebSocket fallback, and so Safari) rejects
`createBidirectionalStream` and `createUnidirectionalStream` at once when
`waitUntilAvailable` is false and the peer has granted no stream credit, as
Chrome's WebTransport does, instead of queueing the create. The change lands
in the in-tree package.

## Plan

Decided 2026-10-07: JS requests pass the flag and otherwise rely on their
10 s timer to bound the queue.
Match the rejection Chrome raises so `@moq/net` handles both the same way.

Decided 2026-10-20 (maintainer): patched in-tree after
[TS qmux](/quest/m1/ts-qmux.md), not upstream in moq-dev/web-transport.

## Required

- [TS qmux](/quest/m1/ts-qmux.md) - `@moq/qmux` lives in `js/`
