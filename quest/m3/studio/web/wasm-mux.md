# [S] hang and moq-mux on wasm

## Goal

`hang` and `moq-mux` build for `wasm32-unknown-unknown`. Their tokio `fs`
feature and file IO sit behind a native-only cfg, which
`rs/moq-wasm/Cargo.toml` names as the prerequisite for browser media muxing.
A CI `cargo check` for wasm32 keeps them building.

## Plan

[moq-video in the browser](/quest/m3/studio/web/video-web.md) is the consumer.
This lands with it or immediately before it.
