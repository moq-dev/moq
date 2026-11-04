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

Complete the PR's local validation and cross-vendor review, and settle the
public error contract below before marking it ready. Keep every existing
test in CI, at least nightly; do not disable a flaky test to satisfy checks.
This follows the user's explicit requirement over the repository's
immediate-disable instruction.

## Waiting

- Maintainer: choose the new public initialization-error names. Recommended:
  keep `moq_quic::ConfigError::Crypto(String)` and
  `moq_tokio::tls::Error::Crypto(String)` as implemented, preserving the
  infallible provider-helper signatures and reporting rind's feature name at
  fallible setup. Alternative: introduce a shared typed crypto error and make
  `crypto::provider()` and `install_default()` fallible, expanding the public
  migration. The existing Plan settles CPU refusal but not these new names
  or helper signatures.

## Related

- [MoQ on rind](/quest/m1/rind/README.md) - the settled provider and CPU support decisions
