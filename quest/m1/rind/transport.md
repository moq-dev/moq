# [S] Finish transport on rind

## Goal

moq-quic, moq-tokio, moq-uring, moq-rtmp, qmux, and moq-relay's own HTTP and
TLS use rind, without selecting aws-lc or ring. Remove moq-tokio's and
moq-quic's provider features and report unsupported CPU features at setup.
No wire format changes.

## Plan

The settled migration is implemented in
[moq-dev/moq#7873](https://github.com/moq-dev/moq/pull/7873), with migration
notes and certificate/provider test sites updated together. rind 0.1.12
supports older x86_64 CPUs without crypto extensions, aarch64 with NEON
(including Pi 4, CM4, and Zero 2 W without crypto extensions), and armv7.
The epic owns the final workspace crypto graph guard; WebRTC consolidation
and signing remain in their separate areas.

Public error contract, decided by the maintainer on 2026-11-04:

- ✅ Introduce a shared typed crypto error, and make `crypto::provider()` and
  `crypto::install_default()` fallible, with MIGRATE notes for the broader
  API migration. This replaces `moq_quic::ConfigError::Crypto(String)` and
  `moq_tokio::tls::Error::Crypto(String)`.
- Not chosen: keeping `Crypto(String)` with the existing infallible
  provider-helper signatures.

Open: where the shared type lives. moq-tokio's `crypto` and `tls` modules
compile without moq-quic (the `net`-only build), so a type defined in
moq-quic cannot be shared with them as the dependency graph stands. The
options are in the PR's `## Decisions needed`.

Complete the PR's local validation and cross-vendor review, and settle that
placement before marking it ready. Keep every existing test in CI, at least
nightly; do not disable a flaky test to satisfy checks. This follows the
user's explicit requirement over the repository's immediate-disable
instruction.

## Related

- [MoQ on rind](/quest/m1/rind/README.md) - the settled provider and CPU support decisions
