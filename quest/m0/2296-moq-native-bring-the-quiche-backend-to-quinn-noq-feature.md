# [L] moq-native: bring the quiche backend to quinn/noq feature parity

## Goal

Implement and verify the behavior tracked in [#2296](https://github.com/moq-dev/moq/issues/2296)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

Audit the quiche backend against the common quinn/noq behavior in `moq-native` and close the remaining gaps.

Audited at `45217596f` with:

- `web-transport-quiche` 0.4.2
- `tokio-quiche` 0.19.1
- `quiche` 0.29.3

WebTransport over HTTP/3 already works. Raw QUIC also works now: `nix develop --command cargo test -p moq-native --all-features quiche_raw_quic -- --ignored --nocapture` passes, so its stale `#[ignore]` is now a coverage gap rather than a backend limitation.

#### Missing parity

##### Protocol coverage and lifecycle

- \[ ] Remove the stale `#[ignore]` from `quiche_raw_quic` and keep raw `moqt://` / `moql://` in normal backend coverage.
- \[ ] Make `Server::close` actually stop the quiche listener and close/drain active connections. `QuicheServer::close` is currently a no-op, while quinn/noq send an endpoint-wide close.

##### TLS and certificates

- \[ ] Support outbound mTLS with `--client-tls-cert` + `--client-tls-key`. The shared rustls config loads them, but the quiche connection path never passes them to `web_transport_quiche::ez::ClientBuilder::with_single_cert`.
- \[ ] Support inbound optional mTLS with `--server-tls-root`, validate the client chain, and expose it through `Request::peer_identity`. The quiche server currently ignores the roots and always returns `None`.
- \[ ] Support `--client-tls-host-name` by decoupling the dial target from TLS SNI/hostname verification. The current quiche builder uses one host for DNS and SNI, so initialization rejects the option.
- \[ ] Match quinn/noq platform certificate verification, including mobile. The current quiche path snapshots `rustls-native-certs`; the in-tree comment notes that iOS/Android get no roots and fail closed, and it does not provide the same OS verifier behavior.
- \[ ] Implement the complete `tls::Server` certificate semantics:

  - load every configured cert/key pair instead of only index 0;
  - include generated certificates alongside file-backed certificates instead of choosing one source;
  - select the certificate by SNI;
  - validate key/certificate matches up front;
  - hot-reload file-backed certificates and update reported fingerprints.

  `web-transport-quiche` now exposes `ServerBuilder::with_cert_resolver`, so SNI selection and reload can be integrated locally.
- \[ ] Honor `SSLKEYLOGFILE` for quiche clients and servers, matching the rustls key logging installed by quinn/noq.

##### QUIC transport controls and routing

- \[ ] Honor `--client-quic-keep-alive` and `--server-quic-keep-alive`. They are currently ignored because the exposed quiche settings have no keep-alive interval.
- \[ ] Allow `--client-quic-gso=false` and `--server-quic-gso=false`. Both currently fail initialization. Server-side capability selection may be possible with a custom `QuicListener`; the client builder currently enables maximum socket capabilities internally.
- \[ ] Honor `--server-preferred-v4` / `--server-preferred-v6`. quiche still has TODOs for encoding/decoding the `preferred_address` transport parameter.
- \[ ] Honor `--server-quic-lb-id` / `--server-quic-lb-nonce` with the same validation and connection-ID layout as quinn/noq. It is currently logged and ignored. `tokio-quiche::QuicListener` exposes a connection-ID generator, so this appears locally implementable.
- \[ ] Use the shared dual-stack UDP binding behavior and address-family-aware DNS selection. The quiche builders currently bind through `std::net::UdpSocket` and select the first DNS result, bypassing `bind::udp` and `util::pick_addr`. This can regress `[::]` plus IPv4 connectivity, especially on Windows.

#### Already at parity

Do not duplicate work for behavior already wired up:

- WebTransport over HTTP/3
- raw QUIC functionality, after removing the stale test ignore
- protocol-version ALPN negotiation
- maximum bidi/uni stream counts
- idle timeout
- path MTU discovery
- custom root certificates
- explicit SHA-256 certificate pinning
- `http://` fingerprint bootstrap
- disabled certificate verification
- terminal HTTP/auth rejection classification
- connection/session statistics

#### Suggested implementation split

Likely local `moq-native` work with APIs already exposed:

- raw-QUIC test coverage
- outbound client certificates
- dynamic SNI certificate resolver and hot reload
- QUIC-LB CID generator
- shared server socket binding
- key logging

Likely upstream `web-transport-quiche` / `tokio-quiche` / `quiche` work, or a local lower-level integration:

- separate dial address and SNI hostname
- server-side client-certificate verification and peer-chain access
- full platform verifier support
- preferred address
- client-side GSO capability override
- explicit listener/active-connection shutdown
- keep-alive support

#### Acceptance criteria

- Every public `moq-native` option that quinn/noq share is either honored by quiche or rejected only for a documented upstream limitation.
- Backend integration tests cover raw QUIC, WebTransport, mTLS/peer identity, hostname override, dual-stack binding, certificate selection/reload, and shutdown.
- Quiche-only caveats can be removed from the public field docs once their corresponding checks pass.

#### Related but not parity blockers

- \#2276 is noq-only multipath, which quinn does not support.
- \#686 tracks congestion control/BBR, where quinn and noq do not currently behave the same.
- \#679 tracks multi-threaded UDP receive scaling, which is a reason to use quiche rather than a parity gap.

## Closes

- [#2296](https://github.com/moq-dev/moq/issues/2296) - close this issue when the quest finishes
