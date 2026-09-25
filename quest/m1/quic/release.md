# [M] Release the QUIC stack

## Goal

Immutable releases of `moq-noq-proto`, `moq-noq`, `moq-noq-udp`,
`web-transport-moq`, and the qmux crate are available to every published MoQ
crate. The workspace lockfile identifies exact released sources, with no
root-only Cargo patch or mutable git branch, and a consumer can tell from any
release which parent commit it carries.

## Plan

Release the dependency chain from the bottom up: the fork's crates, which one
tag releases together, then `web-transport-trait` if its surface moved, then
qmux. Pin each released version in this repository's workspace dependencies
and regenerate `Cargo.lock`. Verify minimal, default, and all-feature builds so
enabling iroh, qmux, or the uring runtime cannot unify two incompatible copies
of the protocol state.

Each fork release documents the parent commit, the carried patches with their
upstream PR or the reason there is none, and the security-update procedure. A
release is incomplete if a consumer cannot tell whether an advisory against
the parent applies.

## Required

- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - preserve the corrected controller in later stack releases

- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - the
  WebTransport-required transport extension
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - the new
  transport API
- [qmux on the QUIC stream state machine](/quest/m1/quic/qmux.md) - the
  shared qmux implementation
