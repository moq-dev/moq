# [M] JavaScript WebSocket to QUIC upgrade

## Goal

A `js/net` `Connection` that came up over the WebSocket fallback migrates to
WebTransport once the WebTransport dial completes: the app-visible handle and
its origins are unchanged, live tracks hand over without a dropped group, the
WebSocket session receives a GOAWAY and drains within the handover cap, and
the "WebSocket won" memo forgets the URL. When WebTransport wins the race
nothing changes.

## Plan

Lands in `js/net`, reusing the GOAWAY handover the drain line shipped once
[JS track handover](/quest/m1/js-group-handover.md) resumes tracks
across it: dial the replacement while the old session keeps serving, swap the origin wiring once it is established, leave the old
session to close on its own or at the handover cap. See the
[questline](/quest/m1/transport-upgrade/README.md) for the shared decisions.

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
- Swap only once the server admits the WebTransport session: mirror Rust's
  `moq_net::Session::accepted()` in `js/net` (resolves on the server's SETUP,
  rejects with the close reason on a refusal) and await it inside the
  connect deadline. A refusal, a stall, or moq-lite-03/-04 (no server SETUP)
  keeps WebSocket with no GOAWAY sent.
- A self-sent GOAWAY must gate new requests on the old session too, not only
  a received one: [JS GOAWAY requests](/quest/m1/js-goaway-requests.md)
  covers the received case, so check it also covers this path.
- On a successful upgrade delete the URL from `websocketWon`.
- Tests in the browser harness against the in-tree relay: with the
  WebTransport dial delayed past the head start, a watched track keeps every
  group across the upgrade, the WebSocket session closes within the cap, and
  the next connect to the same URL gives WebTransport the head start again;
  with no delay, WebTransport wins and no WebSocket session is ever opened.
- Public API: the `accepted` mirror, if it is exported; `transportOf` already
  reports the live transport. Update `doc/lib/js` where the fallback race is
  described.

## Required

- [JS track handover](/quest/m1/js-group-handover.md) - tracks carry across the handover this upgrade reuses without a dropped group
- [JS GOAWAY requests](/quest/m1/js-goaway-requests.md) - no new request opens on a session that is going away
