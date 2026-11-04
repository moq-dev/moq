# [S] @moq/net passes qmux stream limits through

## Goal

`@moq/net` passes `@moq/qmux`'s stream limits (`maxStreamsBidi`,
`maxStreamsUni`) through as session configuration, and both default to
10,000, matching Rust's sessions and the relay. A relay test passes with a
WebSocket JS publisher serving more than 100 live tracks and a WebSocket JS
reader holding more than 100 live groups.

## Plan

Planned 2026-11-04 (maintainer) from Kixel's T3 host, where the 98th live
track over WebSocket was refused with `remote error: 0` and the 100th stalled
on the reader. `js/qmux/src/session.ts` already has the config fields, but its
`DEFAULT_CONFIG` grants only 100 each way, and `@moq/net` never sets them. With
both raised to 10,000 locally, 1,000 live tracks ran with no errors. m0
because every Node or WebSocket JS client fails hard near 100 today.
Browsers' own WebTransport caps are separate: see
[stream credit](/quest/m1/stream-credit/README.md).
