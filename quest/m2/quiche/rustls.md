# [L] rustls crypto backend

## Goal

The fork can handshake with rustls, and `rs/moq-uring` uses it: boringssl is
gone from the io_uring path, and the server-side TLS wart goes with it -
TLS material hot-reloads for new handshakes (quinn/noq parity). The
iOS/Android platform-verifier wart is erased only where a mobile-capable
consumer selects the rustls feature; moq-uring is Linux-only, and the mobile
client path rides tokio-quiche (boring, external), so that half lands when a
client consumer adopts the fork's rustls feature, not in this quest.

Boundary: crypto is a fork FEATURE, not a swap. `boring` stays the default so
upstream compatibility and the tokio-quiche consumer are untouched; only
moq-uring selects `rustls`.

## Plan

- Fork-side work lands in the moq-dev/quiche repository; this quest covers the
  moq-side adoption in this repo plus the fork PRs.
- quiche's handshake and packet protection bottom out in boringssl's SSL
  object today (`Config::with_boring_ssl_ctx_builder` is how moq-uring
  configures it). rustls has a QUIC-native API (`rustls::quic`: key schedule,
  header protection, AEAD) that maps onto the same seam; the port is a crypto
  backend trait inside the fork with boring and rustls implementations.
- Crypto provider selection (aws-lc-rs default, ring optional) follows
  `moq-tokio/src/tls.rs`, which already builds rustls configs for quinn and
  noq; the quiche backend joins the shared config path instead of its parallel
  `CertificateDer` plumbing.
- Hot-reload is not extra work once rustls owns the handshake: the existing
  `notify`-based reload plumbing from the quinn path applies as-is, and lands
  here with tests proving reload on the uring listener. The platform-verifier
  plumbing equally carries over, but only reaches mobile when a client
  consumer adopts the feature (see Goal).
- Upstreaming: assume it does not happen. cloudflare has declined rustls
  backends twice (quiche#129, and quiche#1259 was a direct PR offer refused
  without review); the internal `crypto` seam existing is the only thing in
  our favor. Offer the seam once stable, plan nothing around acceptance.
- The fork half is MERGED:
  [moq-dev/quiche#1](https://github.com/moq-dev/quiche/pull/1) ports
  [quiche#2045](https://github.com/cloudflare/quiche/pull/2045) (hargut's
  declined upstream PR, the most complete prior art anywhere) onto 0.29.3,
  full suite green under boring, rustls-aws-lc-rs, and rustls-ring, and
  survived an adversarial review (resumption, 0-RTT lifetime, SNI, config
  hot-reload, constant-time compares). Remaining: moq-uring adopts the
  feature here, once a quiche release carries the merge, plus the hot-reload
  tests on the uring listener. Deliberate limitations, recorded in the module
  docs: no cross-process session resumption, no per-connection keylog writers
  (SSLKEYLOGFILE works).
- The backends are FEATURE-EXCLUSIVE, enforced by `compile_error!`: cargo
  unifies features across a graph, so a silent priority rule would let any
  crate that enables quiche's defaults (tokio-quiche does) quietly flip the
  backend out from under moq-uring. Consequence for the adoption half: a
  relay build containing both moq-uring (rustls) and moq-tokio's `quiche`
  feature (tokio-quiche, boring) cannot unify. Either the relay drops
  `web-transport-quiche` from its feature set when uring is in play, or the
  fork later grows runtime backend selection; decide when wiring moq-uring.
