<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![Documentation](https://docs.rs/moq-msf/badge.svg)](https://docs.rs/moq-msf/)
[![Crates.io](https://img.shields.io/crates/v/moq-msf.svg)](https://crates.io/crates/moq-msf)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-msf

Catalog types for the MOQT Streaming Format (MSF).

This crate implements the catalog format defined in [draft-ietf-moq-msf-01](https://www.ietf.org/archive/id/draft-ietf-moq-msf-01.txt),
with additional support for CMAF packaging from [draft-ietf-moq-cmsf-00](https://www.ietf.org/archive/id/draft-ietf-moq-cmsf-00.txt).

`Catalog` is a version-agnostic snapshot of tracks: the wire details (the catalog `version` and draft-01's `initDataList`/`initRef` indirection for init data) are handled during (de)serialization. Parsing accepts both draft-00 and draft-01 catalogs and serializing always emits the newest draft, so older publishers remain compatible and callers never touch the version on the wire.

Used by [moq-mux](https://github.com/moq-dev/moq/tree/main/rs/moq-mux) for muxing/demuxing media. For the higher-level [hang](https://github.com/moq-dev/moq/tree/main/rs/hang) catalog format used elsewhere in this repo, see that crate.

See the [API documentation](https://docs.rs/moq-msf/) for details.
