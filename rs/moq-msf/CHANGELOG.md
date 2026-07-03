# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-msf-v0.2.0...moq-msf-v0.3.0) - 2026-06-30

### Other

- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

### Changed

- Track draft-ietf-moq-msf-01, with the wire format hidden behind the API. `Catalog` is now a version-agnostic snapshot (`{ tracks }`); the `version` field and the `Version` enum are gone. Parsing accepts draft-00 (numeric `version`, inline `initData`) and draft-01 (string `version`, root `initDataList` + per-track `initRef`); serializing always emits draft-01. Init data is resolved to inline `Track::init_data` on parse and hoisted into a deduplicated `initDataList` on serialize, so callers never touch the version or the init-data indirection.

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-msf-v0.1.3...moq-msf-v0.2.0) - 2026-05-23

### Added

- Unified CMSF/Hang pipeline (cleanup of #1429) ([#1444](https://github.com/moq-dev/moq/pull/1444))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-msf-v0.1.2...moq-msf-v0.1.3) - 2026-04-19

### Other

- Add README files for Rust crates ([#1284](https://github.com/moq-dev/moq/pull/1284))

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-msf-v0.1.1...moq-msf-v0.1.2) - 2026-04-03

### Other

- Add moq-relay release workflow and Nix cache configuration ([#1178](https://github.com/moq-dev/moq/pull/1178))

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-msf-v0.1.0...moq-msf-v0.1.1) - 2026-03-13

### Other

- Set MSRV to 1.85 (edition 2024) ([#1083](https://github.com/moq-dev/moq/pull/1083))
