# [XS] moq-hls builds without default features

## Goal

`cargo check -p moq-hls --no-default-features --all-targets` passes with
warnings denied. Today moq-hls `Broadcaster::poll_closed`
(`rs/moq-hls/src/export/mod.rs`) is dead under `--no-default-features`: its
only caller is the `server` feature's eviction loop.

## Plan

Gate `poll_closed` on the `server` feature, or move it next to its caller.
Found 2026-10-25 for the feature-shapes quest, which
[moq-dev/moq#6703](https://github.com/moq-dev/moq/pull/6703) finished before it
could carry this fix.

Public API: none. Wire: none.
