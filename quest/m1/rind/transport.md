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

Placement of the shared error, decided by the maintainer on 2026-11-04:

- ✅ In `moq-crypto`, behind a new `provider` feature holding
  `UnsupportedCpu`, `check()`, `provider()` and `install_default()`. It
  depends only on rind and rustls, never on moq-net, so moq-quic, moq-tokio,
  qmux and moq-uring use it without a cycle, and net-only builds gain only
  that feature's dependencies. rind stays private: the error wraps rind's
  `MissingFeature` instead of re-exporting it.
- Not chosen: re-exporting rind's `MissingFeature`; defining the error in
  moq-quic and requiring moq-quic from moq-tokio; a brand-new crate.

Complete the PR's local validation and cross-vendor review before marking
it ready. Keep every existing test in CI, at least nightly; do not disable a
flaky test to satisfy checks. This follows the user's explicit requirement
over the repository's immediate-disable instruction.

## Related

- [MoQ on rind](/quest/m1/rind/README.md) - the settled provider and CPU support decisions
