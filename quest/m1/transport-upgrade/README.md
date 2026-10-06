# WebSocket to QUIC upgrade

## Goal

A session that came up over the WebSocket fallback moves to QUIC once the
QUIC handshake completes, without dropping a group. Today `https://` races
QUIC against WebSocket, WebSocket wins whenever QUIC is slower than the head
start plus a TCP+TLS+upgrade round trip (a lost Initial, a slow first
WebTransport dial), and the loser is closed: the session then spends its whole
life on a reliable transport that cannot shed load under congestion, and the
"WebSocket won" memo removes QUIC's head start from every later connect in
that process or page. The end state: when WebSocket wins, the QUIC dial keeps
going; if it lands, the reconnect loop attaches the QUIC session, hands the
routes over, sends a GOAWAY on the WebSocket session, and drains it. When QUIC wins, WebSocket is closed immediately, as today.

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

Shared decisions:

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
  it enters draining. The relay refuses new requests on it from then on; the
  front cancels its subscriptions once the new session feeds them.
- One-shot `connect()` returns one session and never upgrades; every
  `Connection` upgrades, reconnecting or not.
- Publishing over the old session is announced again over the new one; the
  relay's route order prefers the newest route, and an anonymous client's
  per-session origin makes that a replacement rather than a join, which is
  immediate either way.

## Required

- [JavaScript](/quest/m1/transport-upgrade/js.md) - js/net keeps the WebTransport dial after WebSocket wins and migrates through the client-goaway handover
- [Closed fallback](/quest/m1/transport-upgrade/closed-fallback.md) - a WebSocket session that closes right after connecting falls back to the pending QUIC dial instead of redialing
- [JS qmux finish](/quest/m1/transport-upgrade/js-qmux-finish.md) - `@moq/qmux` reports a cleanly finished send stream as closed without error
