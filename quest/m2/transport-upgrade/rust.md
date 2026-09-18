# [M] Rust WebSocket to QUIC upgrade

## Goal

A `moq_tokio::Connection` that came up over the WebSocket fallback migrates to
QUIC once the QUIC dial completes: live tracks hand over at a group boundary,
the WebSocket session receives a GOAWAY and drains within the handover cap,
`Status::Migrating` is observable across the swap, and the "WebSocket won"
memo forgets the URL. When QUIC wins the race nothing changes.

## Plan

Lands on `dev`, in `rs/moq-tokio`. See the
[questline](/quest/m2/transport-upgrade/README.md) for the shared decisions.

- `race_moq_connect` (`rs/moq-tokio/src/client.rs`) currently drops the losing
  arm. When WebSocket wins, return the handshaken WebSocket session together
  with the pending QUIC future; when QUIC wins or the QUIC arm has already
  failed, return the session alone. `Client::dial` keeps its one-session shape
  for the one-shot `connect()` path; give `Connection` the variant that carries
  the pending upgrade.
- `Connection::run` (`rs/moq-tokio/src/connection.rs`) polls the pending QUIC
  dial alongside `run_session`, the way it already polls `draining`. When it
  completes, run the MoQ handshake on it with the same `moq_net::Client` (the
  origins attach and the front reselects to the newest route), call
  `shared.migrating()` then `shared.connected(&new)`, send
  `old.drain().send(Goaway::same().timeout(handover))` with the configured
  `GoawayConfig` cap, and move the old session into `Draining`. A QUIC dial
  that fails after WebSocket won is logged at debug, leaves the memo untouched,
  and the session carries on over WebSocket. The pending dial is dropped when the WebSocket session ends
  first; the reconnect races again.
- On a successful upgrade remove the URL from `WEBSOCKET_WON`
  (`rs/moq-tokio/src/websocket.rs`).
- Add `moq_tokio::Transport` (QUIC, WebTransport, WebSocket, TCP, Unix) and
  `Connection::transport()` returning it for the live session, so telemetry can
  see the swap. It lives in `moq-tokio`, not `moq_net`: the session is generic
  over the transport trait and cannot know what it runs on, only the dial
  does. Mirrors `js/net`'s `transportOf`.
- Regression test against the in-tree relay: the QUIC path goes through an
  in-process UDP forwarder that delays packets past the WebSocket head start so
  WebSocket wins deterministically; a subscribed track keeps every group across
  the swap, `Status::Migrating` is observed, the WebSocket session closes
  within the handover cap, and a second connect to the same URL gives QUIC the
  head start again. A second test drops the delay to zero and asserts QUIC wins
  and no second session is ever opened.
- Public API: additive (the transport accessor). Wire: none; the client GOAWAY
  is already specified for either endpoint on moq-lite 04+ and an empty-URI
  GOAWAY is legal for a moq-transport client. Update `doc/lib/rs` where the
  fallback race is described.
