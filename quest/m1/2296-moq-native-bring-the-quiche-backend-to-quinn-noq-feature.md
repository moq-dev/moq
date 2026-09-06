# [M] moq-tokio: bring the quiche backend to quinn/noq feature parity

## Goal

Close the remaining gaps between the quiche backend and the common quinn/noq
behavior in `moq-tokio` (the crate `moq-native` was renamed to), tracked by
[#2296](https://github.com/moq-dev/moq/issues/2296).

## Plan

Most of the original audit has landed. What remains is listed below, each with
what blocks it. Re-audit against the current tree before starting: the issue was
written against `moq-native` and several items closed themselves as dev moved.

### Already done

Do not redo these; they have tests.

- WebTransport over HTTP/3, raw QUIC (`moqt://` / `moql://`), and the `http://`
  fingerprint bootstrap.
- Protocol-version ALPN negotiation, stream counts, idle timeout, path MTU
  discovery, custom roots, explicit SHA-256 pinning, disabled verification,
  terminal HTTP/auth rejection, connection statistics.
- Outbound mTLS from `--connect-tls-cert` / `--connect-tls-key`, and from an
  in-memory `tls::Identity`.
- Inbound optional mTLS from `--listen-tls-root`, surfaced through
  `Request::peer_identity`.
- `--connect-tls-host-name`, keep-alive, `--*-quic-gso=false`, and the shared
  dual-stack bind plus address-family-aware DNS selection.
- The full `tls::Server` certificate semantics, through the shared
  `tls::ServeCerts` and `ez::ServerBuilder::with_cert_resolver`: every
  configured cert/key pair, generated and in-memory certificates alongside the
  file-backed ones, SNI selection, key/certificate validation, and hot reload
  with live fingerprints.

### Remaining

Local work, with the APIs already exposed:

- \[ ] Honor `--listen-quic-lb-id` / `--listen-quic-lb-nonce` with the same
  validation and connection-ID layout as quinn/noq. Currently logged and
  ignored. `tokio_quiche::QuicListener` carries a `cid_generator`, and
  `ez::ServerBuilder::with_listener` takes such a listener, so this is
  implementable here. Note that `with_listener` bypasses `with_gso`: the
  listener's `capabilities` have to be computed by the caller, so GSO must keep
  working through that path.

Blocked on `web-transport-quiche` / `tokio-quiche` / `quiche`, or on a local
lower-level integration that skips their `ez` layer:

- \[ ] Make `Server::close` stop the quiche listener and close/drain active
  connections. `QuicheServer::close` is a no-op, while quinn/noq send an
  endpoint-wide close. `ez::Server` neither exposes a close nor tracks its
  established connections.
- \[ ] Match quinn/noq platform certificate verification, including mobile.
  boringssl takes a concrete root list rather than a rustls verifier, so the
  client path snapshots `rustls-native-certs`; iOS/Android get no roots and fail
  closed.
- \[ ] Honor `SSLKEYLOGFILE`, matching the rustls key logging quinn/noq install.
  The `SslContextBuilder` is built inside `web-transport-quiche`'s connection
  hook, which exposes no keylog callback.
- \[ ] Hot reload the inbound mTLS client roots (`--listen-tls-root`).
  `ez::ClientAuth` is applied once, when the listener is built.
- \[ ] Support a pinned client-fingerprint allowlist (`tls::Listen::peers`),
  which currently returns `tls::Error::PeersUnsupported`. It needs a per-handshake
  verify callback on the server side; boringssl's client-auth path validates
  against a fixed root store instead.
- \[ ] Honor `--listen-preferred-v4` / `--listen-preferred-v6`. quiche still has
  TODOs for encoding/decoding the `preferred_address` transport parameter.

### Not parity blockers

- \#2276 is noq-only multipath, which quinn does not support.
- \#686 tracks congestion control/BBR, where quinn and noq do not currently
  behave the same.
- \#679 tracks multi-threaded UDP receive scaling, which is a reason to use
  quiche rather than a parity gap.

## Closes

- [#2296](https://github.com/moq-dev/moq/issues/2296) - close this issue when the quest finishes
