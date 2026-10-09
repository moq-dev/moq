# [S] qmux refuses over-limit creates without waiting

## Goal

`@moq/qmux` (the WebSocket fallback, and so Safari) rejects
`createBidirectionalStream` and `createUnidirectionalStream` at once when
`waitUntilAvailable` is false and the peer has granted no stream credit, as
Chrome's WebTransport does, instead of queueing the create. Released from
moq-dev/web-transport and bumped in `js/net/package.json`.

## Plan

Decided 2026-10-07 with [JS requests](/quest/m1/js-request-deadline.md),
which pass the flag and otherwise rely on their 10 s timer to bound the queue.
Match the rejection Chrome raises so `@moq/net` handles both the same way.
