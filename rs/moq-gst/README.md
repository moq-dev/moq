<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-gst

A [GStreamer](https://gstreamer.freedesktop.org/) plugin for [Media over QUIC](https://moq.dev), exposing `moqsink` (and friends) as native GStreamer elements.

Uses [hang](https://github.com/moq-dev/moq/tree/main/rs/hang), [moq-mux](https://github.com/moq-dev/moq/tree/main/rs/moq-mux), and [moq-native](https://github.com/moq-dev/moq/tree/main/rs/moq-native) under the hood, so it can publish CMAF/fMP4 produced by any GStreamer pipeline directly to a MoQ relay.

This crate is not published to crates.io; build it from this repo.

## Building

```bash
cargo build --release -p moq-gst
```

The resulting plugin is at `target/release/libgstmoq.so` (or `.dylib` / `.dll` on macOS / Windows). Point `GST_PLUGIN_PATH` at the containing directory to make it discoverable by `gst-inspect-1.0` and the rest of GStreamer.
