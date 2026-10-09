# WebSocket to QUIC upgrade

## Goal

A session that came up over the WebSocket fallback moves to QUIC once the
QUIC handshake completes, without dropping a group. `https://` races QUIC
against WebSocket, and WebSocket wins whenever QUIC is slower than the head
start plus a TCP+TLS+upgrade round trip (a lost Initial, a slow first
WebTransport dial). The end state: when WebSocket wins, the QUIC dial keeps
going; if it lands, the reconnect loop attaches the QUIC session, hands the
routes over, sends a GOAWAY on the WebSocket session, drains it, and forgets
the "WebSocket won" memo. When QUIC wins, WebSocket is closed immediately.

The Rust `moq_tokio::Connection` does this since #4180
(`websocket_upgrades_to_quic`, `a_refused_upgrade_keeps_websocket` in
`rs/moq-tokio/src/connection.rs`). What remains is the `js/net` half, where
the loser is still closed and the memo never forgotten, plus the two
children below.

Non-goals: migrating between IPv6 and IPv4. That race is inside the QUIC dial,
before any MoQ session exists, the loser has no claim to being the better path
(RFC 8305 takes the first to complete and never migrates), the browser does
not expose it, and a family preference belongs to QUIC path migration rather
than a second MoQ session. No flap guard and no opt-out: a QUIC session that
completes the handshake and then dies goes through the ordinary reconnect
backoff, which races again.

## Plan

The upgrade is a self-initiated migration, so it reuses the peer-GOAWAY
machinery rather than adding a second handover path.
`moq_tokio::Connection` already dials a replacement while a `Draining` handle
keeps the old session serving until it closes or overstays the handover cap,
reports `Status::Migrating`, and `moq_net::Session::drain()` sends a GOAWAY on
every version (a client may send one with an empty URI; only a redirect URI is
forbidden to a moq-transport client). The origin's multi-route front prefers
the newest of two equal routes, and since #4741 a front resumes each track
from the new route's copy at the first frame the subscriber lacks
(`model/resume.rs`), cancelling the old session's subscription once the new
one feeds it. The JavaScript
GOAWAY handover landed with the drain line, but it does not resume tracks yet;
the JS half requires [JS track handover](/quest/m1/js-group-handover.md) for
that.

Shared decisions, which Rust implements and [JavaScript](/quest/m1/transport-upgrade/js.md) mirrors:

- The race returns the winner plus the still-pending QUIC dial when WebSocket
  wins. The attempt's connect deadline bounds that dial; no extra deadline.
- The swap waits for the peer's SETUP on the QUIC session
  (`moq_net::Session::setup`) within that same deadline; until
  then the WebSocket session gets no GOAWAY. A refused or stalled QUIC session
  is dropped and WebSocket keeps serving. moq-lite-03 and -04 carry no server
  SETUP, so they never upgrade. This crate's servers send SETUP after admission;
  other servers may send it before admission and still refuse afterward.
- On a successful upgrade the "WebSocket won" memo (`WEBSOCKET_WON` in
  `moq-tokio`, `websocketWon` in `js/net`) forgets the URL: QUIC works on this
  network, so the head start comes back. Otherwise a network where WebSocket
  narrowly beats QUIC would open two connections on every reconnect.
- The old session gets `Goaway::new()` with the configured handover cap before
  it enters draining. The relay prices its routes at the drain cost and keeps
  opening requests on it only until the new session's route wins; the front
  cancels its subscriptions once the new session feeds them.
- One-shot `connect()` returns one session and never upgrades; every
  `Connection` upgrades, reconnecting or not.
- Publishing over the old session is announced again over the new one; the
  relay's route order prefers the newest route, and an anonymous client's
  per-session origin makes that a replacement rather than a join, which is
  immediate either way.

## Required

- [JavaScript](/quest/m1/transport-upgrade/js.md) - js/net keeps the WebTransport dial after WebSocket wins and migrates through the client-goaway handover
- [Closed fallback](/quest/m1/transport-upgrade/closed-fallback.md) - a WebSocket session that closes right after connecting falls back to the pending QUIC dial instead of redialing
