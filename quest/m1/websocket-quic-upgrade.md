# [M] A WebSocket fallback session returns to QUIC once UDP works

## Goal

A reconnecting session that fell back to WebSocket re-probes QUIC and
migrates back once UDP works. A redial right after an outage prefers QUIC
instead of letting WebSocket win the race. One brief UDP outage no longer
leaves a link on WebSocket indefinitely.

Non-goals: no new transport, and no change to how downstream deployments
alert on a degraded link.

## Plan

Planned 2026-11-08 with the maintainer as a follow-up of
[moq-dev/moq.pro#3742](https://github.com/moq-dev/moq.pro/pull/3742).
Milestone m1 (maintainer): additive, no semver break.

What #3742 saw: on moq.pro's wtf fleet (pinned at `f48bc940d`), a short UDP
outage made a relay mesh link redial, WebSocket won the redial race, and the
link then stayed on WebSocket. moq.pro's mesh-link alert treats a WebSocket
mesh link as degraded, so it paged `still unhealthy after …` until something
else forced a redial. On a production fleet, every brief blip would turn into
a page that never clears. `main` at `f6a0e549d` has no re-probe:
`rs/moq-net/src/reconnect.rs` re-dials only after a session ends, and
`rs/moq-tokio/src/websocket.rs` documents that a session that lands on
WebSocket stays on it for its lifetime. The QUIC-vs-WebSocket race
(`race_moq_connect` in `rs/moq-tokio/src/client.rs`) gives QUIC a head start
(`--connect-websocket-delay`), but a WebSocket win records the server in
`WEBSOCKET_WON`, and every later dial there skips that head start until QUIC
wins once. JS keeps the same memory (`websocketWon` in
`js/net/src/connection/connect.ts`). So once a redial during the outage
lands on WebSocket, every later redial there races with no head start, and
only a QUIC win clears the memory.

The work:

- **Post-outage race.** Make the redial prefer QUIC without slowing the
  first connect to a target that really has no UDP. Start from the
  `WEBSOCKET_WON` memory: it was added so a UDP-blocked target doesn't pay
  the head start on every dial, but after an outage it is stale. Confirm with
  a test that it is why WebSocket keeps winning, then bound it so the bound
  takes effect before the redial: clear the entry when the WebSocket session
  that set it ends (no clock), or expire it. Clearing it once a probe reaches
  QUIC is only cleanup: a QUIC race win already clears it, and a probe
  succeeds only after the redial has raced.
  Recommended: keep the race as it is apart from that, and
  treat a WebSocket win as provisional.
- **Re-probe and migrate.** While a session runs on the WebSocket fallback
  for a target whose URL also allows QUIC, probe QUIC with backoff. Once a
  probe succeeds, move to a QUIC session, make-before-break, the way the
  GOAWAY handover (`Reconnect::predecessor`) already overlaps two sessions,
  so subscriptions don't drop. Recommended: reuse the handover rather than
  add a second migration path. Bound the probe rate so a target with no UDP
  doesn't pay for constant handshakes.
- Mirror it in JS (`js/net`): `websocketWon` and the reconnecting
  connection in `js/net/src/connection/reload.ts` have the same fallback.
  Where the browser path differs, note why in the PR.
- Tests on the mocked clock: block UDP, let the link fall back to
  WebSocket, unblock it, and assert that the link is on QUIC within the
  probe bound with no dropped subscription. Assert that a target with no
  UDP stays on WebSocket and probes no faster than the bound.

Update the `websocket.rs` module docs, which promise a WebSocket session
stays on WebSocket for its lifetime.

Public API: possibly a probe interval setting under `[connect.websocket]`,
next to `--connect-websocket-delay`. If added, mirror it in `moq-ffi`'s
`MoqWebSocketConfig` (`rs/moq-ffi/src/session.rs`) beside `enabled` and
`delay`, so every SDK gets it.
Wire: none.

## Related

- [WebSocket enforces bitrate caps](/quest/m3/rate-websocket.md) - the WebSocket fallback path this moves sessions off
