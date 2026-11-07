# [S] Transports take shared types

## Goal

- moq-uring stops defining its own congestion and TLS types. Its QUIC
  configs take a `moq_congestion` factory (BBR3 by default, matching
  moq-tokio; today `moq_uring::quic::Congestion` defaults to `Loss`) and an
  `Arc<rustls::ClientConfig>` or `Arc<rustls::ServerConfig>`, as moq-tokio's
  do. `Identity`, `ClientAuth`, and the client TLS fields are gone.
  `moq_uring::quic::Congestion` stays as a convenience that converts into a
  factory, so the built-in families keep their names while the config holds
  only a factory. The relay's io_uring runtime installs the controller a
  `moq_tokio::quic::Config::controller` carries instead of refusing it
  (`rs/moq-tokio/src/quic.rs`), so an embedder gets the same controller on
  either runtime.
- qmux names no tungstenite type: TLS is qmux's own `tls::Client` config,
  `WebSocketConfig` folds into qmux's record-size limits, `Upgraded::new`
  takes the raw upgraded IO plus a role, `Error::WebSocket` is opaque, and
  `pub use tokio_tungstenite` goes (`rs/qmux/src/ws.rs`), so a tungstenite
  major is not a qmux break.
- moq-tokio's `server::Request::quic()` is removed; nothing outside the
  crate calls it, and it exposes the QUIC backend through the session API.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests): ✅ the
`Congestion` enum converts to a factory, and the config takes only a factory
(rejected: delete the enum; keep the enum with a factory variant). This
reconciles the 2026-11-01 decision that the enum keeps its built-in families
with one factory both runtimes install, and absorbs the deleted m3
uring-controller quest. qmux's tungstenite exposure is from
[the stream codec bench](/quest/m4/perf/stream/codec.md), which stays
perf-only.

Found by the 2026-11-07 API shape audit (transport); see the
[epic](/quest/m2/api-shape/README.md). The relay's io_uring path builds its
configs the way the tokio path does, so it loses its conversion code. The
moq-quic config knobs and re-export fan-out are in
[moq-quic-unused-surface](/quest/m2/moq-quic-unused-surface.md), and the
shared sans-IO driver in [quic-sansio-driver](/quest/m3/quic-sansio-driver.md).
Run the uring benches before and after, since the default controller
changes.
