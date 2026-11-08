# [S] Transports take shared types

## Goal

- moq-uring stops defining its own congestion and TLS types. Its QUIC
  configs take a `moq_congestion` factory (BBR3 by default, matching
  moq-tokio; today `moq_uring::quic::Congestion` defaults to `Loss`) and an
  `Arc<rustls::ClientConfig>` or `Arc<rustls::ServerConfig>`, as moq-tokio's
  do. `moq_uring::quic::Congestion`, `Identity`, `ClientAuth`, and the client
  TLS fields are gone.
- moq-tokio's `server::Request::quic()` is removed; nothing outside the
  crate calls it, and it exposes the QUIC backend through the session API.

## Plan

Found by the 2026-11-07 API shape audit (transport); see the
[epic](/quest/m2/api-shape/README.md). The relay's io_uring path builds its
configs the way the tokio path does, so it loses its conversion code. The
moq-quic config knobs and re-export fan-out are in
[moq-quic-unused-surface](/quest/m2/moq-quic-unused-surface.md), and the
shared sans-IO driver in [quic-sansio-driver](/quest/m3/quic-sansio-driver.md).
Run the uring benches before and after, since the default controller
changes.
