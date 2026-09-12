# [S] A connect fails on auth only once every transport has

## Goal

`moq_tokio::Client::connect` against a WebTransport-only endpoint succeeds
whenever the QUIC dial succeeds, even when the WebSocket fallback is refused
first. Cloudflare's relays answer every non-WebTransport request with 403, and
today that 403 landing inside the 200 ms fallback delay fails the whole connect
as Forbidden while the QUIC arm is still in flight, indistinguishable from a bad
token. An auth error ends the race only when the other arm has also failed.

Boundaries: blind subscription, consuming a broadcast the peer never announced,
stays refused. A client here may hold several connections and cannot pick one
for a bare path, so the announcement is the signal a broadcast is online; a
relay that accepts SUBSCRIBE_NAMESPACE and never publishes a namespace is
non-conformant, not a gap in this repo.

## Plan

Branch from dev: `rs/moq-tokio` exists only there, and main still ships the
deprecated `rs/moq-native`.

`race_transport_connect` (`rs/moq-tokio/src/client.rs:642`) polls both arms
in a `select!` loop. Each arm has an early return on `err.is_auth()`: the QUIC
arm at line 660 and the WebSocket arm at line 671. Every other failure is
recorded and the loop keeps polling the other arm, then line 690 folds the
pair: both failed is `Error::TransportRace`, one failed is that error, neither
ran is `ConnectFailed`. `connect_inner` calls it at line 564 with the
WebSocket arm from `websocket::race_handle` (`rs/moq-tokio/src/websocket.rs:228`),
which yields `None` when the fallback is disabled and otherwise dials after the
configured head start, so a fast 403 is what lands first.

- Delete both `is_auth()` early returns. An auth error is recorded like any
  other failure and the loop continues until both arms are done.
- Fold the pair with auth awareness: the result is an auth error only when
  both arms rejected authentication. When one arm is auth and the other is a
  non-auth failure, report the non-auth failure, which stays retryable. This
  matters because the reconnect loop treats `is_auth()` as terminal at
  `rs/moq-tokio/src/connection.rs:759` and `:810`; a fallback endpoint that
  refuses every non-WebTransport request must not stop the client from
  redialing a QUIC arm that merely failed transiently.
- Keep `TransportRace` for the both-non-auth case so the log still names
  both failures.

Tests, all in the `client.rs` test module beside the existing three:

- Flip `race_transport_connect_stops_on_quic_auth_error` (line 1228): a QUIC
  `Unauthorized` followed by a WebSocket success now connects over WebSocket.
- WebSocket 403 (`ConnectError::Forbidden` or the status mapping in
  `rs/moq-tokio/src/connect.rs:366`) then QUIC success connects over QUIC.
- Both arms refusing reports an auth error.
- WebSocket 403 then a transient QUIC failure reports the QUIC error, and
  `err.is_auth()` is false so `Reconnect` retries.
- `race_transport_connect_keeps_websocket_after_quic_non_auth_error` (1242)
  and `race_transport_connect_returns_when_quic_transport_connects` (1252)
  pass unchanged.

## Closes

- [#3532](https://github.com/moq-dev/moq/issues/3532) - close this issue when the quest finishes

## Related

- [FFI WebSocket fallback](/quest/m2/ffi-websocket-fallback.md) - moq-ffi and its wrappers gain the disable and delay setters the CLI and libmoq already have
