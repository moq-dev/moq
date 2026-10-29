# [XS] moq-quic's all-features rustdoc builds

## Goal

`cargo doc -p moq-quic --all-features` succeeds; today it fails on
`SentPackets`, already broken on `main` before
[moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950).

## Plan

Fix the doc build. The gate's Rust lint builds workspace rustdoc without
`--all-features`, and `rs doctest` runs `cargo test --doc`, so nothing builds
this. Add `cargo doc -p moq-quic --all-features` to Nightly's feature lane
(`just rs features`), and name the lane in the PR.

Public API: none.
