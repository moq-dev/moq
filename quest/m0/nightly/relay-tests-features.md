# [XS] moq-relay tests build without default features

## Goal

`cargo clippy -p moq-relay --tests --no-default-features` passes with
warnings denied. Its integration tests use `listen::Config.unix` and
`connect::Config.websocket` only under the features that define them.

## Plan

Seven `E0609`s, in `rs/moq-relay/tests/integration/{smoke,auth_lifetime,cluster_unknown,public_rules,session_revalidate}.rs`.
Gate each use (or the tests that need the transport) on its feature instead
of turning the features on for every test build.

No CI lane builds this shape today. Workspace unification enables moq-relay's
defaults through moq-cli, and Nightly runs
`cargo check -p moq-relay --no-default-features` without `--tests`. Decided
2026-10-26 (maintainer): fix it here on its own; the per-crate lane in
[Feature shapes per crate](/quest/m0/nightly/hls-poll-closed-gate.md) keeps it
fixed once that lands.

Public API: none. Wire: none.
