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

Decided 2026-10-31 (maintainer):

- Like every area, this waits on rind's aarch64 path for CPUs without the
  crypto extensions, so moq-cli never stops starting on a Pi 4, CM4, or
  Zero 2 W running a 64-bit OS.
- moq-hls returns an unsupported-CPU error from the constructors that first
  need rind (such as `Broadcaster::new` and `Server::new`), rather than
  panicking later or relying on a documented startup check. Like moq-e2ee,
  the error carries rind's message, not rind's type, per `rs/AGENTS.md`.

Public API: moq-hls constructors that need rind become fallible (breaking;
handle the new error). Wire: none.

## Required

- [rind runs on a Pi 4's aarch64 CPU](/quest/m2/rind/aarch64.md) - the rind release this switches to
