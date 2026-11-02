# Threat model: moq

## What this project does and where untrusted input enters

moq (Media over QUIC) is a live media pub/sub protocol stack. `moq-relay` is a public server: clients connect over QUIC (WebTransport or raw QUIC) or WebSocket, publish broadcasts made of tracks, groups and frames, and subscribe to other clients' broadcasts through the relay's cache and fan-out. Relays also connect to each other to form a cluster. The `moq` CLI (`moq-cli`) publishes and consumes media and runs ingest gateways (RTMP, RTSP, SRT, WebRTC WHIP/WHEP, HLS/DASH) that translate other protocols into MoQ.

Treat everything a peer sends as hostile, before and after authentication:

- **QUIC**: `moq-quic` is our sans-IO QUIC state machine (forked from quinn-proto). Packets, frames, transport parameters, stream and flow-control state, and handshake data all arrive before any authorization.
- **The MoQ wire protocol** in `moq-net`: SETUP and version negotiation, then control and data messages for both moq-lite (`src/lite`) and the IETF moq-transport drafts (`src/ietf`). This covers varints, paths and path patterns, announce, subscribe, fetch, group and object headers, priorities, and the timing and ordering of all of them.
- **WebSocket and HTTP** on the relay's `[web]` listeners: the upgrade, the public `/announced`, `/fetch` and `/certificate.sha256` endpoints, and query strings.
- **Authorization inputs**: the JWT (`moq-auth`), the SETUP token, the dialed path and query, and the relay's handling of grants (root, publish and subscribe pattern unions, mounts, expiry, revalidation).
- **Media payloads**: catalogs (`hang`, `moq-msf`, JSON in `moq-json`) and containers and codecs parsed by `moq-mux` (fMP4, MPEG-TS, FLV, MKV, LOC, and the H.264, H.265, AV1, VP8, VP9, AAC, Opus and FLAC bitstream parsers). A publisher controls these bytes, and the relay or a subscriber may parse them.
- **Ingest gateways**: RTMP (`moq-rtmp`), RTSP (`moq-rtsp`), SRT (`moq-srt`), WebRTC and ICE/STUN (`moq-rtc`, `moq-ice`), and HLS playlists and segments (`moq-hls`).
- **Peer relays** in a cluster are authenticated but should not be able to break a node beyond what their grant allows.

## Security properties the relay must hold

The relay is deployed on the public internet as shared infrastructure: one process serves many unrelated tenants, most of them anonymous or holding a token from a third party's auth server. Attack these properties:

- **Untrusted network input, bounded cost.** Whatever a peer sends, the memory, CPU, and state it makes the other side hold must be bounded. A peer that can grow relay memory or CPU without bound, past the configured per-session limits (`rs/moq-relay/src/limits.rs`), is a bug even when it takes many messages to do. Fan-out is the multiplier to look for: one publisher's bytes, or one subscriber's request, reaching many sessions or pinning cache.
- **Encryption.** Every transport is TLS 1.3 via rustls with aws-lc: QUIC and WebTransport, and WebSocket over `[web.https]`. Certificates are loaded from disk, issued by ACME (`rs/moq-relay/src/acme.rs`, TLS-ALPN-01), or self-signed and pinned by the fingerprint at `/certificate.sha256`. Check whether the relay accepts 0-RTT, and if it does, whether early data can be replayed. Plain `[web.http]` is a documented development option, not a finding.
- **Authentication.** Sessions are admitted by a JWT (`moq-auth`, which accepts HMAC, RSA, ECDSA and EdDSA keys, so algorithm confusion and key selection are in scope), by an external auth server over HTTP (`doc/bin/relay/auth.md`), by a verified client certificate weighed by that server, or by a static anonymous grant. Expiry, revalidation, and in-band renewal must never extend a grant past what the server issued, and a failure to reach the auth server must refuse rather than admit.
- **Multi-tenancy.** A grant's `root` scopes a session to one subtree; `mounts` add read-only views. A session must never publish, subscribe, receive announcements, fetch, or see stats outside its grant, whether through path normalization, pattern matching (`moq-pattern`), the HTTP endpoints, cluster forwarding, or a revalidation that changes the grant mid-session. One tenant must not be able to degrade another beyond its fair share, for example by exhausting a shared cache or a shared connection limit.
- **Cache integrity.** The relay caches and forwards groups by broadcast, track, and group. A publisher must not be able to overwrite or inject into a broadcast it was not granted, and a subscriber must never receive one tenant's data under another tenant's name.
- **Cluster trust.** Peer relays authenticate with tokens or mTLS and get broader rights than clients. A relay dials only configured or discovered peers, never a URL learned from an announcement. LAN discovery over mDNS (`[cluster.lan]`) is open to anyone on the network unless `secret` is set, which is documented; with `secret` set, an unkeyed host must not be able to join.
- **Amplification and reflection.** QUIC's anti-amplification limit and address validation, the opt-in public STUN responder (`rs/moq-relay/src/stun.rs`), and ICE in the WebRTC gateway all answer unauthenticated UDP. A response larger than the request toward a spoofed source is a finding.
- **End-to-end encryption.** `moq-e2ee` encrypts groups, datagrams, and track names so a relay cannot read them. A flaw in its construction (nonce reuse, a key or name leaking to the relay, a forgeable group) is in scope.

## Components that matter most / least

Most important, in this order:

1. `rs/moq-relay` and the crates it serves the network with: `rs/moq-net`, `rs/moq-quic`, `rs/moq-tokio`, `rs/moq-uring`, `rs/moq-sock`, `rs/moq-auth`, `rs/moq-pattern`.
2. Authorization: a session publishing, subscribing, or seeing announcements outside its grant, or reaching another root (tenant) through path tricks, revalidation, mounts, or cluster links.
3. `rs/moq-mux` and `rs/hang`: container, codec, and catalog parsing of publisher-controlled bytes.
4. The ingest gateways: `rs/moq-rtmp`, `rs/moq-rtsp`, `rs/moq-srt`, `rs/moq-rtc`, `rs/moq-ice`, `rs/moq-hls`.
5. `rs/moq-e2ee`, `rs/moq-cli`, `rs/moq-json`, `rs/moq-stats`, `rs/kio`.

`unsafe` is rare and worth reading closely where it appears: `rs/moq-quic`, `rs/moq-net/src/model`, `rs/moq-net/src/util/cell.rs`, `rs/moq-net/src/transport.rs`, and `rs/moq-relay/src/uring.rs`.

Out of scope: `js/` and the other language bindings (`py/`, `swift/`, `kt/`, `go/`, `dart/`, `cpp/`, `rs/moq-ffi`, `rs/moq-c`), capture and encode crates that read local devices (`moq-video`, `moq-v4l`, `moq-nvenc`, `moq-audio`, `moq-transcode`, `moq-gst`, `moq-boy`), `demo/`, `bench/`, `test/`, `infra/`, `doc/`, `drafts/`, and CI configuration. A flaw that lies wholly inside a dependency (quinn, rustls, aws-lc, tokio, hyper, str0m) belongs upstream unless our use of it makes it reachable.

## How to exercise it

The image is offline, with every crate fetched and `CARGO_NET_OFFLINE` set. The debug build of the in-scope crates, their test binaries, and the `moq-relay` and `moq` binaries are already built in `/src/target/debug`. Debug builds keep overflow checks on.

- **Tests**: `cargo nextest run -p path+file:///src/rs/moq-net` (or any in-scope crate; the path spec is needed because the lockfile also has published copies of some crates). Use nextest as CI does: some tests install process-global state and fail under plain `cargo test`. Add `--features moq-relay/io-uring` for the relay and `moq-uring` so the io\_uring backend's tests run (they skip on a kernel without io\_uring support). Unit tests use the deterministic clock in `kio::clock` rather than wall time, so they are fast and reproducible.
- **A relay to attack**: `./target/debug/moq-relay demo/relay/localhost.toml` listens on UDP and TCP `[::]:4443` with a self-signed certificate and anonymous access to every path. The binary is built with io\_uring support; pass `--runtime-io-uring` to serve QUIC from the io\_uring workers in `rs/moq-uring` and `rs/moq-relay/src/uring.rs` instead of tokio. For authorization testing, grant a narrow anonymous pattern with `[auth] public_subscribe` / `public_publish`, or run the reference auth server (`moq auth generate`, `moq auth sign`, `moq auth serve`, then `[auth] url`; see `doc/bin/cli.md`) and try to escape a signed token's root and patterns. `doc/bin/relay/` documents the config, auth, cluster, and HTTP endpoints.
- **A client**: `./target/debug/moq --help` publishes, subscribes, fetches, and lists announcements against a relay. `curl http://localhost:4443/announced/` and `curl http://localhost:4443/fetch/<broadcast>/<track>` hit the HTTP endpoints.
- **Fuzz harnesses**: `rs/moq-net/fuzz` and `rs/moq-quic/fuzz` hold cargo-fuzz targets for the wire codecs and the QUIC state machine. cargo-fuzz and a nightly toolchain are not in the image, but the targets show the parser entry points.

## How you rate severity

- **Critical**: memory corruption or code execution reachable by an unauthenticated peer; any way to publish to, subscribe to, or enumerate paths outside a session's grant, including reaching another root; accepting a forged, tampered, or expired credential; reading or altering another session's traffic, or `moq-e2ee` plaintext, without its keys.
- **High**: memory corruption that needs an authenticated session; a panic, abort, deadlock, or unbounded memory or CPU growth in `moq-relay` that one unauthenticated peer can trigger, since it takes down every session on the node; one tenant serving data under another tenant's broadcast name.
- **Medium**: the same relay denial of service when it needs an authenticated session; resource use that exceeds the configured limits by a large constant factor; a panic in a gateway or parser on publisher-controlled media; UDP amplification toward a spoofed address; an unkeyed host joining a `secret`-protected LAN cluster.
- **Low**: a crash or hang in a client (`moq` CLI, a subscriber) caused by a malicious relay or publisher; leaking non-secret metadata.

Not a vulnerability: bandwidth exhaustion by simply sending a lot of data within limits, generous defaults an operator can lower, the anonymous `**` grant in `demo/relay/localhost.toml`, and plain-HTTP token exposure (documented as dev-only).

## How reports and patches should look

Give the smallest reproducer: a Rust test against the in-crate API or a sequence of wire bytes, plus the command that triggers it. Patches should fix the root cause and test the edge case, rather than add a retry, sleep, or timeout.
