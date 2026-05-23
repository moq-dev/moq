<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-loc/badge.svg)](https://docs.rs/moq-loc/)
[![Crates.io](https://img.shields.io/crates/v/moq-loc.svg)](https://crates.io/crates/moq-loc)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-loc

Frame encoding for the Low Overhead Container (LOC) defined in
[draft-ietf-moq-loc](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html).

LOC packs a small set of property Key-Value-Pairs (timestamp, optional per-frame
timescale, etc.) in front of a raw codec bitstream payload. This crate handles
just the wire encoding; for catalog-driven dispatch see
[moq-mux](https://github.com/moq-dev/moq/tree/main/rs/moq-mux).

See the [API documentation](https://docs.rs/moq-loc/) for details.
