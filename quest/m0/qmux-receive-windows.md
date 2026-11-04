# [S] @moq/net's WebSocket receive windows match Rust

## Goal

`@moq/net`'s WebSocket fallback gives each qmux session the same receive
windows Rust's `moq-tokio` uses (4 MiB per connection, and its per-stream
default), configurable beside `websocket.maxStreams`, with a measured
throughput comparison before and after recorded in the PR.

## Plan

Planned 2026-11-04 (maintainer, m0) from
[qmux stream limits](/quest/m0/qmux-stream-limits.md)
([moq-dev/moq#7893](https://github.com/moq-dev/moq/pull/7893)): `@moq/net`
still uses `@moq/qmux`'s defaults (`receiveWindow` 1 MiB per connection, 256
KiB per stream, in `js/qmux/src/session.ts`), while `moq-tokio` uses
`DEFAULT_RECEIVE_WINDOW` 4 MiB (`rs/moq-tokio/src/quic.rs`, sized for about
270 Mbps at its RTT). Nothing fails, but WebSocket JS throughput is capped
below Rust's. Measure a WebSocket JS reader's throughput through a relay at a
few RTTs, then match Rust's defaults and expose them on the same
`ConnectProps.websocket` settings as `maxStreams`, with the same validation.
Keep `@moq/qmux`'s own defaults unchanged, as #7893 did for stream limits.
