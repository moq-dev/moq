# [M] JavaScript WebSocket to QUIC upgrade

## Goal

A `js/net` `Connection` that came up over the WebSocket fallback migrates to
WebTransport once the WebTransport dial completes: the app-visible handle and
its origins are unchanged, live tracks hand over without a dropped group, the
WebSocket session receives a GOAWAY and drains within the handover cap, and
the "WebSocket won" memo forgets the URL. When WebTransport wins the race
nothing changes.

## Plan

Lands in `js/net`, after the [client goaway](/quest/next/drain/client-goaway.md)
quest ships the handover it reuses: dial the replacement while the old session
keeps serving, swap the origin wiring once it is established, leave the old
session to close on its own or at the handover cap. See the
[questline](/quest/next/transport-upgrade/README.md) for the shared decisions.

- `connectInner` (`js/net/src/connection/connect.ts`) currently resolves
  `cancel` once one transport is ready, which closes the loser. When WebSocket
  wins, leave the WebTransport attempt running and return it alongside the
  established session; when WebTransport wins, cancel WebSocket as today. The
  one-shot `connect()` export keeps returning one `Established` and closes the
  pending attempt.
- The reconnecting `Connection` awaits the pending WebTransport `ready`, runs
  the same `connectTransport` handshake on it, and feeds it into the GOAWAY
  handover as if the WebSocket session had been told to migrate with an empty
  URI: origins swap, the old session sends an empty-URI GOAWAY on every wire that
  carries the message (lite 04+ and every IETF draft; a moq-transport client
  may not name a redirect URI, and an empty one is legal) and closes at the
  configured cap. A WebTransport attempt that fails after WebSocket won is logged at
  debug and the session stays on WebSocket.
- On a successful upgrade delete the URL from `websocketWon`.
- Tests in the browser harness against the in-tree relay: with the
  WebTransport dial delayed past the head start, a watched track keeps every
  group across the upgrade, the WebSocket session closes within the cap, and
  the next connect to the same URL gives WebTransport the head start again;
  with no delay, WebTransport wins and no WebSocket session is ever opened.
- Public API: none beyond what client-goaway adds; `transportOf` already
  reports the live transport. Update `doc/lib/js` where the fallback race is
  described.

## Required

- [Client goaway](/quest/next/drain/client-goaway.md) - the handover this upgrade reuses
