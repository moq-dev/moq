# [XS] Clean finish in @moq/qmux

## Goal

`@moq/qmux` reports a send stream that finished cleanly as closed without
error, so `@moq/net` does not treat a delivered GOAWAY or SETUP as a failure
over the WebSocket fallback.

## Plan

The Rust qmux crate reported every finished stream as `connection closed`
(fixed in moq-dev/web-transport#402). Check whether the TypeScript peer in
`moq-dev/web-transport` (`js/qmux`) has the same bug, and whether `@moq/net`
waits on a finished writable anywhere it would notice. If it does, fix it
there with a regression test, release, and bump `@moq/qmux` in `js/net`. If
not, note that in the PR that deletes this quest.

Public API: none. Wire: none.

## Related

- [JavaScript upgrade](/quest/m1/transport-upgrade/js.md) - sends a GOAWAY over the WebSocket session on every upgrade
