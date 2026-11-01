# [L] Transport on rind

## Goal

moq-quic, moq-tokio, moq-uring, moq-rtmp, qmux, and moq-relay's own HTTP and
TLS take all their crypto from
rind and `rind-rustls`, their graphs hold no aws-lc-sys or ring, and moq-tokio's
and moq-quic's `aws-lc-rs` and `ring` features are gone. Every binary built on
moq-tokio checks rind's CPU support at startup.

## Plan

Decided with the maintainer on 2026-10-26; see the
[epic](/quest/m2/rind/README.md).

- moq-quic's direct calls move to rind's API: stateless reset HMAC-SHA256 and
  its HKDF-SHA256 key derivation, address-validation tokens (HKDF to
  AES-256-GCM), and Retry integrity (AES-128-GCM). Packet keys and header
  protection come through rustls's QUIC traits from `rind-rustls`. The
  provider is no longer picked per feature.
- moq-tokio installs `rind-rustls`, generates self-signed certificates through
  `rind-rcgen`, drops rustls's default features and rustls-webpki's `aws-lc-rs`,
  and moves mDNS's HMAC-SHA256 from RustCrypto to rind.
- moq-uring builds its configs from `rind-rustls` instead of hardcoding
  aws-lc's provider.
- moq-rtmp's handshake HMAC-SHA256 moves to rind, and its rustls stops forcing
  aws-lc. qmux's TLS takes `rind-rustls`; tungstenite's own `sha1` stays.
- reqwest and axum-server switch to their no-provider features wherever these
  crates enable them. Decided 2026-10-31 (maintainer): this includes
  moq-relay's own reqwest, axum-server, and rustls `aws-lc-rs` features, which
  no other area owns; relay ACME still owns rustls-acme.
- The roughly 100 test call sites that install aws-lc's provider install
  rind's.
- The startup check runs where moq-tokio's crypto is first configured, and its
  error names the missing CPU feature. Every binary inherits it, including
  moq-ffi.
- Docs: the CPU requirement goes into the relay, CLI, and library docs, and
  the removed features leave `doc/lib`.

Public API: moq-tokio's and moq-quic's `aws-lc-rs` and `ring` features are
removed (breaking; migrate by dropping them, since rind is always on).
Unsupported CPUs now fail at startup. Wire: none.

## Required

- [rind runs on a Pi 4's aarch64 CPU](/quest/m2/rind/aarch64.md) - the rind release this switches to
- [moq-quic's crypto is rustls](/quest/m1/quic-api/crypto.md) - the sealed crypto surface rind plugs into, without the per-provider gates
