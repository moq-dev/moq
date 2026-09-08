# [S] A connect fails on auth only once every transport has

## Goal

`moq_native::Client::connect` against a WebTransport-only endpoint succeeds
whenever the QUIC dial succeeds, even when the WebSocket fallback is refused
first. Cloudflare's relays answer every non-WebTransport request with 403, and
today that 403 landing inside the 200 ms fallback delay fails the whole connect
as Forbidden while the QUIC arm is still in flight, indistinguishable from a bad
token. An auth error ends the race only when the other arm has also failed, and
a moq-ffi caller can disable the WebSocket fallback the way libmoq and the CLI
already can.

Boundaries: blind subscription, consuming a broadcast the peer never announced,
stays refused. A client here may hold several connections and cannot pick one
for a bare path, so the announcement is the signal a broadcast is online; a
relay that accepts SUBSCRIBE_NAMESPACE and never publishes a namespace is
non-conformant, not a gap in this repo.

## Plan

- `race_transport_connect` in `rs/moq-native/src/client.rs` returns on
  `err.is_auth()` from either arm. Record an auth error like any other failure
  and keep polling the other arm. When both are done, the result is an auth
  error only if both arms rejected authentication; otherwise report the
  non-auth failure, which stays retryable, because `Reconnect::run` exits on
  `is_auth()` and a fallback endpoint that answers every non-WebTransport
  request with 403 must not stop the client from retrying a QUIC dial that
  merely failed transiently. Flip
  `race_transport_connect_stops_on_quic_auth_error` and add: WebSocket 403 then
  QUIC success connects; both arms refusing reports Forbidden; a transient QUIC
  failure after a WebSocket 403 reports the QUIC error and the reconnect loop
  retries.
- `MoqClient` in `rs/moq-ffi/src/session.rs` gains `set_websocket_enabled` and
  `set_websocket_delay` beside `set_tls_disable_verify`, mapping onto
  `websocket::Client::{enabled, delay}`. libmoq already exports
  `moq_client_set_websocket_enabled` and `_delay`; mirror the two setters in
  `py/moq-rs`, `swift`, `kt`, `go/wrapper/moq`, `dart/moq`, and
  `doc/lib/{py,swift,kt,go,dart}`.

On main, additive.

## Closes

- [#3532](https://github.com/moq-dev/moq/issues/3532) - close this issue when the quest finishes
