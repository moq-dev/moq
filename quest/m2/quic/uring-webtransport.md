# [XL] Cut moq-uring WebTransport over to the selected QUIC core

## Goal

Browser WebTransport sessions on `moq-uring` use the selected Quinn-family
core with production parity. The relay defaults to that path, and the custom
moq-dev/quiche fork is retired without removing the separately supported
tokio-quiche backend.

## Plan

Layer the existing HTTP/3 and WebTransport session machinery over the raw
adapter rather than forking a second QUIC driver. Preserve origin and path
validation, HTTP rejection semantics, draft negotiation, datagrams, stream
limits, reliable delivery of every reset stream's WebTransport header,
certificate reload, mTLS peer identity, graceful close, and qlog.

Exercise Chrome through `just test wasm`, the TypeScript interop cases, the
full native smoke matrix, and connection draining during relay restart. Run
the same relay benchmarks as the raw quest with browser-compatible traffic.
Record any feature that cannot match quiche before switching the default.

After parity and benchmark gates pass, remove the production dependency on the
moq-dev/quiche fork and delete fork-only configuration. Keep the ordinary
quiche backend only if it still provides a supported user-selectable path.
Update the backend docs and feature matrix in the same PR.

## Required

- [Drive raw QUIC from the selected core in moq-uring](/quest/m2/quic/uring-raw.md) -
  the packet and stream driver must be proven before HTTP/3 is layered on it
- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - WebTransport
  requires `RESET_STREAM_AT` for reset data streams
