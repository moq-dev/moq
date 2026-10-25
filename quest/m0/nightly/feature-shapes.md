# [S] Feature shapes in the gate

## Goal

Code that is dead under one of a crate's feature shapes fails the PR that
adds it, instead of breaking Nightly's Features job the next morning.

## Plan

Decided 2026-10-24 (maintainer). The Features job ("Check feature and
profile variants") has failed every night on a different feature-gated item:
`moq-net/src/model/bandwidth.rs` (10-22), `moq-net/src/transport.rs` (10-23),
and now `Connection::open_request`
(`rs/moq-tokio/src/quic/driver/connection.rs:378`), which
[moq-dev/moq#6409](https://github.com/moq-dev/moq/pull/6409) added without
`#[cfg(feature = "net")]`; only `quic/web/session.rs` uses it. Reproduce:
`cargo clippy -p moq-tokio --all-targets --no-default-features --features driver,aws-lc-rs -- -D warnings`.

- Fix `open_request`'s gating, and moq-hls `Broadcaster::poll_closed`
  (`rs/moq-hls/src/export/mod.rs`), which is dead under
  `--no-default-features` (found 2026-10-25).
- Add the feature and profile variant check to the PR gate, scoped to the
  crates a branch changes the way `just check` selects them. Measure its
  cost in the PR. Nightly keeps the full matrix.

Public API: none. Wire: none.
