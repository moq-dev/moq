# [S] A relay drain counts web connections from accept

## Goal

A relay drain never exits under an HTTP/1.1 WebSocket client that is still
in TLS or HTTP header parsing. HTTP/1.1 web connections hold the drain from
TCP accept until their first request shows they are not a WebSocket upgrade.
h2 connections release at ALPN, since h2 can't carry a WebSocket today.

## Plan

`serve_ws` (`rs/moq-relay/src/websocket.rs`) takes `activity.enter()` in the
axum handler, so a connection before that point isn't counted and
`Activity::close()` can see zero (the Codex follow-up on #5286).
`doc/bin/relay/config.md` already claims accepted connections hold the
drain, so it is wrong for this case today.

`listener/idle.rs` already sees accept, each request's headers and the
upgrade flag, and its deadline covers TLS. Hold the `Active` there and hand
it to `serve_ws` through a request extension. h2 extended CONNECT isn't
enabled, so an h2 connection releases its guard once ALPN picks h2.

Decided 2026-10-12:

- On HTTP/1.1, release at the first request's headers unless it is an
  upgrade. A later upgrade on the same keep-alive connection is counted from
  the handler only, as today.
- A connection that arrives after the drain commits (`enter()` returns
  None) is accepted without a guard: `serve_ws` refuses the upgrade with 503
  as today, and plain HTTP is served until the listener stops.

Plain HTTP/1.1 on the public listeners (HLS, API, health checks) now holds the
drain from accept through its first request's headers, bounded by
`listen.timeout` and the drain deadline. Say so in `config.md`. The internal
listener doesn't use the web acceptor and is unaffected.

Tests (paused time): start the drain while one HTTP/1.1 client is mid-TLS or
mid-headers; `Relay::run` doesn't return until it upgrades and gets a
GOAWAY, or the deadline passes. A plain HTTP/1.1 GET releases the guard so the drain
exits. `Relay::with_web` (embedders) goes through the same acceptor.

Public API: none. Wire: none.
