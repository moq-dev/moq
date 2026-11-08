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
a page that never clears. `main` at `f6a0e549d` has no re-probe either:
`rs/moq-net/src/reconnect.rs` re-dials only after a session ends, and the
connect race in `rs/moq-tokio/src/connect.rs` (`DEFAULT_RACE`, the WebSocket
fallback) carries no memory of which transport the target supports.

The work:

- **Post-outage race.** Find why WebSocket wins right after an outage, for
  example QUIC's handshake still timing out on a path that has just
  recovered while TCP connects at once. Make the redial prefer QUIC without
  slowing the first connect to a target that really has no UDP. Recommended:
  keep the race as it is, and treat a WebSocket win as provisional.
- **Re-probe and migrate.** While a session runs on the WebSocket fallback
  for a target whose URL also allows QUIC, probe QUIC with backoff. Once a
  probe succeeds, move to a QUIC session, make-before-break, the way the
  GOAWAY handover (`Reconnect::predecessor`) already overlaps two sessions,
  so subscriptions don't drop. Recommended: reuse the handover rather than
  add a second migration path. Bound the probe rate so a target with no UDP
  doesn't pay for constant handshakes.
- Mirror it in JS (`js/net`) where a reconnecting client has the same
  fallback, or note in the PR why the browser path differs.
- Tests on the mocked clock: block UDP, let the link fall back to
  WebSocket, unblock it, and assert that the link is on QUIC within the
  probe bound with no dropped subscription. Assert that a target with no
  UDP stays on WebSocket and probes no faster than the bound.

Public API: possibly a probe interval setting next to `--connect-race`.
Wire: none.

## Related

- [WebSocket enforces bitrate caps](/quest/m3/rate-websocket.md) - the WebSocket fallback path this moves sessions off
