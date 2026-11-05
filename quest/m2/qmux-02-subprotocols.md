# [S] Relays and moq-tokio offer versioned qmux-02 first

## Goal

The relay's WebSocket listener and moq-tokio's WebSocket, TCP, and Unix
clients and servers advertise versioned `qmux-02.{alpn}` subprotocol pairs
ahead of qmux-01, from one shared list. A JS client, which already offers
qmux-02 first, then negotiates qmux-02 against our relay. Older peers still
fall back to 01 or 00 as today.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7664](https://github.com/moq-dev/moq/pull/7664)): milestone m2.

- Decision prompt (maintainer, 2026-11-03): what this quest does about
  qmux-02:
  - ✅ Advertise 02 first, in the relay and moq-tokio, from one shared list.
  - Advertise 02 after 01.
  - Leave the choice to this quest.
- Today `rs/moq-relay/src/websocket.rs:230` lists
  `QMUX_VERSIONS = [QMux01, QMux00]`, and qmux-02 is accepted only as a bare
  fallback. moq-tokio pins `QMux01` in `websocket.rs:548`, `tcp.rs`,
  `unix.rs:16`, `server.rs`, `connection.rs`, and `webrtc.rs`.
  `js/qmux/src/session.ts` already offers `qmux-02.` pairs first.
  `Version::QMux02` exists in Rust (`rs/moq-quic/src/connection/mux/tests.rs`).
- Keep the per-ALPN floor (`QMUX01_ONLY_ALPNS`: moq-transport-18 and newer
  need qmux-01 or later), and extend it to allow 02.
- Move the version list into one place that the relay and moq-tokio share,
  as the comment beside `QMUX01_ONLY_ALPNS` asks.
- Test: a JS and a Rust client each negotiate qmux-02 against the relay and
  a moq-tokio server. A client that offers only qmux-01 still connects.

Public API: none expected. Wire: newer qmux preferred where both sides
support it.

## Related

- [Retire the web-transport JS qmux](/quest/m2/release/qmux-js-retire.md) - the old JS qmux publisher
