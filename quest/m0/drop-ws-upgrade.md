# [M] A connection keeps the transport that won the race

## Goal

A `moq-tokio` connection whose `https://` race is won by WebSocket stays on
WebSocket for that session's lifetime; nothing migrates it to QUIC. The
pending QUIC dial is cancelled the moment WebSocket wins, as when QUIC wins.
An epochless viewer no longer sees a `Restart` shortly after connecting.

Non-goals: changing the race itself, its head start, or the "WebSocket won"
memo's effect on later connects.

## Plan

Decided with the maintainer on 2026-10-13: remove the WebSocket-to-QUIC
upgrade from #4180. On an epochless route, which is the default for
lite-06, the browser, and the bindings, the upgrade is a `Restart`: players
re-resolve, re-fetch the catalog, and reset decoders right after connecting.
That is a brutal user experience, and nothing shows the upgrade helps outside
CI's debug builds. Rejected: upgrading only when epochs match (keeps the
machinery for a minority of sessions), keeping it as is, and measuring first.

- Delete the upgrade in `rs/moq-tokio` (`connection.rs`'s `upgrade` state and
  its handover, `client.rs`'s post-race `Setup` stage) and its tests
  (`websocket_upgrades_to_quic`, `a_refused_upgrade_keeps_websocket`).
- Cancel the pending QUIC dial at once when WebSocket wins, deleting #5284's
  closed fallback too: a WebSocket session that closes early goes through the
  ordinary reconnect, which races again.
- Keep the "WebSocket won" memo (`WEBSOCKET_WON`). Both transports still race
  without the head start, so QUIC can still win a later connect. Its only
  eviction was a successful upgrade; check what clears it now and document it.
- Remove `moq_net::Session::setup()` unless the GOAWAY drain migration gates
  on it; if it does, keep it and say why in its doc. Keep
  `Connection::transport()`, which reports the live transport.
- Keep the origin fix from #5443 (`Release` in `model/origin.rs`): GOAWAY
  drains and publisher replacement still hand an epochless route over the
  same way.
- `js/net` keeps closing the loser as today. This plan's PR deleted the m1
  transport-upgrade epic and its JavaScript child, and folded the QUIC-wins
  race flake in below.
- Add a regression: when WebSocket wins, the QUIC dial is dropped and no
  second session ever starts, with mocked time.
- Absorbs the rest of the QUIC-wins race flake: `quic_winning_opens_one_session`
  already holds the fallback handshake, but `fallback.accepted.try_recv()`
  only proves the queue is empty at one instant. Observe the loser's
  cancellation directly, by a drop or connection-close signal, in both
  directions.
- Update docs and comments that describe the upgrade inline, and search the
  bindings for the `WebTransport` transport report #4180 added; it stays.

Public API: breaking in moq-net (`Session::setup()` removed, unless kept as
above). Behavior: no QUIC upgrade after a WebSocket win. Wire: none. Report
both in the PR.

## Related

- [#4180](https://github.com/moq-dev/moq/pull/4180) - the upgrade this removes
- [#5284](https://github.com/moq-dev/moq/pull/5284) - the closed fallback this removes
- [#5443](https://github.com/moq-dev/moq/pull/5443) - the origin fix that stays
- [Restart followers](/quest/m1/restart-followers.md) - consumers that follow the `Restart` GOAWAY drains and publisher replacement still send
