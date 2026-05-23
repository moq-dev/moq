<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-mux/badge.svg)](https://docs.rs/moq-mux/)
[![Crates.io](https://img.shields.io/crates/v/moq-mux.svg)](https://crates.io/crates/moq-mux)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-mux

Media muxers and demuxers for [Media over QUIC](https://moq.dev). Takes
containerized or raw-codec media in, produces a [hang](https://github.com/moq-dev/moq/tree/main/rs/hang) broadcast — or the other way around.

**Containers:** fMP4 / CMAF, MKV / WebM, HLS, LOC, hang Legacy.
**Codecs:** H.264, H.265, AV1, AAC, Opus.

See the [API docs](https://docs.rs/moq-mux/) for the full module map.
