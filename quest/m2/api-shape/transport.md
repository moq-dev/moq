# [M] qmux names no tungstenite type

## Goal

qmux names no tungstenite type: TLS is qmux's own config (or the shared
`Arc<rustls::ClientConfig>`), `WebSocketConfig` folds into qmux's record-size
limits, `Upgraded::new` takes the raw upgraded IO (its `connect` and `accept`
already name the WebSocket role), `Error::WebSocket` is opaque, and
`pub use tokio_tungstenite` goes (`rs/qmux/src/ws.rs`), so a tungstenite
major is not a qmux break.

## Plan

The moq-uring and `Request::quic_limits` halves of this quest landed in
[moq-dev/moq#8300](https://github.com/moq-dev/moq/pull/8300). This half is
larger than it looked: moq-tokio's `websocket` module runs its own
tungstenite handshakes (a client over a pre-dialed stream with a TLS-name
override, and a server whose header callback captures the URL and picks the
subprotocol) and hands qmux the finished `WebSocketStream`, and
`moq_tokio::websocket::accept` is itself generic over a tungstenite
`Stream + Sink`. A raw-IO `Upgraded` cannot take a socket whose handshake
already finished.

qmux's tungstenite exposure is from
[the stream codec bench](/quest/m4/perf/stream/codec.md), which stays
perf-only. Found by the 2026-11-07 API shape audit; see the
[epic](/quest/m2/api-shape/README.md).

## Waiting

- Maintainer: how moq-tokio keeps its custom handshakes once qmux hides
  tungstenite. Options: (a) qmux's `ws::Client` and `ws::Server` grow entries
  over a caller's stream that expose the request URL and offered protocols
  for the caller to answer, and moq-tokio drops its own handshake code;
  (b) moq-tokio depends on tokio-tungstenite itself and does only the HTTP
  upgrade, handing qmux the raw IO after the 101. Recommend (b): moq-tokio
  already owns that policy, the relay's hyper path already works this way,
  and qmux's API shrinks to raw IO.
