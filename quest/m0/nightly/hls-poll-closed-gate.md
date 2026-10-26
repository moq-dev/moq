# [XS] Feature shapes are checked per crate

## Goal

`cargo check -p moq-hls --no-default-features --all-targets` passes with
warnings denied, and the PR gate checks each selected crate's feature shapes
in isolation, so dead code behind a feature fails the PR that adds it.

## Plan

moq-hls `Broadcaster::poll_closed` (`rs/moq-hls/src/export/mod.rs:164`) is
dead under `--no-default-features`: its only caller is the `server` feature's
eviction loop. Gate it with `#[cfg(feature = "server")]`.

Neither the gate from
[moq-dev/moq#6703](https://github.com/moq-dev/moq/pull/6703) nor Nightly
catches it: workspace feature unification turns `server` on, because moq-cli
depends on moq-hls with it. Only an isolated moq-hls build shows the warning.
Make the gate build each selected crate's feature shapes on its own (`-p` per
crate, not one unified build), and measure the added cost in the PR.

Found 2026-10-25 for the feature-shapes quest, which #6703 finished first.

Public API: none. Wire: none.
