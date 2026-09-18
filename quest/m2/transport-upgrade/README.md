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
routes over at a group boundary, sends a GOAWAY on the WebSocket session, and
drains it. When QUIC wins, WebSocket is closed immediately, as today.

Non-goals: migrating between IPv6 and IPv4. That race is inside the QUIC dial,
before any MoQ session exists, the loser has no claim to being the better path
(RFC 8305 takes the first to complete and never migrates), the browser does
not expose it, and a family preference belongs to QUIC path migration rather
than a second MoQ session. No flap guard and no opt-out: a QUIC session that
completes the handshake and then dies goes through the ordinary reconnect
backoff, which races again.

## Plan

Everything below describes `dev`, which is where both quests land: `main`
still has `moq-native`'s close-only `Reconnect` and no origin routing table in
`js/net`.

The upgrade is a self-initiated migration, so it reuses the peer-GOAWAY
machinery rather than adding a second handover path. On `dev`,
`moq_tokio::Connection` already dials a replacement while a `Draining` handle
keeps the old session serving until it closes or overstays the handover cap,
reports `Status::Migrating`, and `moq_net::Session::drain()` sends a GOAWAY on
every version (a client may send one with an empty URI; only a redirect URI is
forbidden to a moq-transport client). The origin's multi-route front prefers
the newest of two equal routes and `resume` splices each track at a group
boundary, capping the old segment so the old session's subscription ends at the
boundary on its own. The JavaScript handover is the
[client goaway](/quest/m2/drain/client-goaway.md) quest's, so the JS half
requires it.

Shared decisions:

- The race returns the winner plus the still-pending QUIC dial when WebSocket
  wins. The QUIC handshake timeout bounds that dial; no extra deadline.
- On a successful upgrade the "WebSocket won" memo (`WEBSOCKET_WON` in
  `moq-tokio`, `websocketWon` in `js/net`) forgets the URL: QUIC works on this
  network, so the head start comes back. Otherwise a network where WebSocket
  narrowly beats QUIC would open two connections on every reconnect.
- The old session gets `Goaway::same()` with the configured handover cap before
  it enters draining. The relay refuses new requests on it from then on; the
  splice ends its subscriptions at the boundary.
- One-shot `connect()` returns one session and never upgrades; every
  `Connection` upgrades, reconnecting or not.
- Publishing over the old session is announced again over the new one; the
  relay's route order prefers the newest route, and an anonymous client's
  per-session origin makes that a replacement rather than a join, which is
  immediate either way.

## Quests

- [Rust](/quest/m2/transport-upgrade/rust.md) - moq-tokio keeps the QUIC dial after WebSocket wins and migrates through the existing Draining path
- [JavaScript](/quest/m2/transport-upgrade/js.md) - js/net keeps the WebTransport dial after WebSocket wins and migrates through the client-goaway handover

## Related

- [Drain](/quest/m2/drain/README.md) - the peer-initiated half of the same handover
- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - the same race function on main, auth handling only
