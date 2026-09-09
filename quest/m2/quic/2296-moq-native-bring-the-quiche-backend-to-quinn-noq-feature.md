# [L] moq-tokio: bring the quiche backend to quinn/noq feature parity

## Goal

Every `listen`/`connect` setting the quinn and noq backends honor is either
honored by the quiche backend in `moq-tokio` or refused with an error naming
the gap, and the quiche backend can serve a reuseport worker group.

## Plan

Most of the original audit has landed. What remains is listed below, each with
what blocks it.

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

### Connection IDs

The pinned `web-transport-quiche` 0.7 (Cargo.toml:209) exposes
`ez::ServerBuilder::with_listener(tokio_quiche::socket::QuicListener)`
(server.rs:112), and that listener carries a public
`cid_generator: Arc<dyn ConnectionIdGenerator>` (tokio-quiche 0.19.1,
socket/listener.rs:58). The hook exists. `QuicheServer::new` never uses it:
it builds `ServerBuilder::default().with_settings(..).with_gso(..)`
(rs/moq-tokio/src/quiche.rs:686-688), warns and ignores `lb_id` (:642-644),
drops `lb_nonce` (rs/moq-tokio/src/listen.rs:124) without a word, and refuses
any worker `Member` with `Error::ShardUnsupported` (:108, :651) behind a
comment (:647-650) that predates the hook. One change covers all three:

- Build the `QuicListener` locally with a `cid_generator` that lays out
  connection IDs exactly as quinn/noq do (QUIC-LB server id plus nonce, or the
  worker index for a shard member), and hand it to `with_listener`.
- `with_listener` bypasses `with_gso`: the listener's `capabilities` are the
  caller's to compute, so `--listen-quic-gso=false` must keep working through
  that path.
- Retire `ShardUnsupported` once a member binds; the parity gap it names is
  what keeps `--runtime-workers` quinn/noq-only today.

### Remaining, blocked on `web-transport-quiche` / `tokio-quiche` / `quiche`

Or on a local lower-level integration that skips their `ez` layer:

- Make `Server::close` stop the quiche listener and close/drain active
  connections. `QuicheServer::close` is a no-op, while quinn/noq send an
  endpoint-wide close. `ez::Server` neither exposes a close nor tracks its
  established connections.
- Match quinn/noq platform certificate verification, including mobile.
  boringssl takes a concrete root list rather than a rustls verifier, so the
  client path snapshots `rustls-native-certs`; iOS/Android get no roots and fail
  closed.
- Honor `SSLKEYLOGFILE`, matching the rustls key logging quinn/noq install.
  The `SslContextBuilder` is built inside `web-transport-quiche`'s connection
  hook, which exposes no keylog callback.
- Hot reload the inbound mTLS client roots (`--listen-tls-root`).
  `ez::ClientAuth` is applied once, when the listener is built (quiche.rs:705).
- Support a pinned client-fingerprint allowlist (`tls::Listen::peers`),
  which currently returns `tls::Error::PeersUnsupported`. It needs a
  per-handshake verify callback on the server side; boringssl's client-auth
  path validates against a fixed root store instead.
- Honor `--listen-preferred-v4` / `--listen-preferred-v6`. quiche still has
  TODOs for encoding/decoding the `preferred_address` transport parameter.

### Not parity blockers

- #2276 is noq-only multipath, which quinn does not support.
- #686 tracks congestion control/BBR, where quinn and noq do not currently
  behave the same.
- #679 tracks multi-threaded UDP receive scaling, which is a reason to use
  quiche rather than a parity gap.

## Required

- [noq parity gate](/quest/m2/quic/noq-parity.md) - decides whether quiche stays a supported backend; if it is retired this quest is abandoned with the verdict
- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Closes

- [#2296](https://github.com/moq-dev/moq/issues/2296) - close this issue when the quest finishes
