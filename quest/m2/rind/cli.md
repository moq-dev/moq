# [S] moq-cli and moq-hls on rind

## Goal

moq-cli's S3, GCS, and Azure storage, its HTTPS server, and moq-hls's HTTP
client and segment naming take their crypto from rind, so neither graph holds
aws-lc-rs.

## Plan

Decided with the maintainer on 2026-10-26; see the
[epic](/quest/m2/rind/README.md).

- object_store drops its `aws-lc-rs` feature and takes `rind-object-store` as
  its `CryptoProvider` (HMAC-SHA256 and SHA-256 for S3 and Azure, RSA
  PKCS#1-SHA256 for GCS).
- reqwest and axum-server switch to their no-provider features, with rind's
  provider installed.
- moq-hls's segment and rendition names hash SHA-256 through rind instead of
  RustCrypto `sha2`, with identical names.

Public API: none. Wire: none.
