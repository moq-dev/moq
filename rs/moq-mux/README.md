<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux/)
[![Crates.io](https://img.shields.io/crates/v/moq-mux.svg)](https://crates.io/crates/moq-mux)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-mux

Media muxers and demuxers for [Media over QUIC](https://moq.dev), bridging containerized media into [hang](https://github.com/moq-dev/moq/tree/main/rs/hang) broadcasts.

Supported formats:

- **fMP4 / CMAF** (`mp4` feature)
- **HLS** (`hls` feature)
- **MSF** — see [moq-msf](https://github.com/moq-dev/moq/tree/main/rs/moq-msf) for the catalog types

Supported codecs:

- **Video:** H.264, H.265, AV1
- **Audio:** AAC, Opus

See the [API documentation](https://docs.rs/moq-mux/) for details.
