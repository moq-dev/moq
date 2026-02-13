# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.8](https://github.com/moq-dev/moq/compare/libmoq-v0.2.7...libmoq-v0.2.8) - 2026-02-12

### Other

- Error cleanup ([#944](https://github.com/moq-dev/moq/pull/944))
- Reduce the moq-lite API size ([#943](https://github.com/moq-dev/moq/pull/943))

## [0.2.7](https://github.com/moq-dev/moq/compare/libmoq-v0.2.6...libmoq-v0.2.7) - 2026-02-09

### Other

- Use `moq` instead of `hang` for some crates ([#906](https://github.com/moq-dev/moq/pull/906))
- Remove priority from the catalog ([#905](https://github.com/moq-dev/moq/pull/905))

## [0.2.6](https://github.com/moq-dev/moq/compare/libmoq-v0.2.5...libmoq-v0.2.6) - 2026-02-03

### Other

- updated the following local packages: moq-lite, hang

## [0.2.5](https://github.com/moq-dev/moq/compare/libmoq-v0.2.4...libmoq-v0.2.5) - 2026-01-24

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- Add universal libmoq build for macos  ([#861](https://github.com/moq-dev/moq/pull/861))
- Add #[non_exhaustive] to moq-native configuration. ([#850](https://github.com/moq-dev/moq/pull/850))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))

## [0.2.4](https://github.com/moq-dev/moq/compare/libmoq-v0.2.3...libmoq-v0.2.4) - 2026-01-12

## [0.2.3](https://github.com/moq-dev/moq/compare/libmoq-v0.2.2...libmoq-v0.2.3) - 2026-01-10

### Added

- iroh support ([#794](https://github.com/moq-dev/moq/pull/794))

### Other

- Add generic time system with Timescale type ([#824](https://github.com/moq-dev/moq/pull/824))
- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))
- target_link_libraries ([#802](https://github.com/moq-dev/moq/pull/802))

## [0.2.2](https://github.com/moq-dev/moq/compare/libmoq-v0.2.1...libmoq-v0.2.2) - 2025-12-19

### Other

- Add HLS import module ([#789](https://github.com/moq-dev/moq/pull/789))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/libmoq-v0.1.0) - 2025-12-13

### Other

- Use BufList for hang::Frame ([#769](https://github.com/moq-dev/moq/pull/769))
- Fix and over-optimize the H.264 annex.b import ([#766](https://github.com/moq-dev/moq/pull/766))
- Don't use 0 index for the slab. ([#758](https://github.com/moq-dev/moq/pull/758))
- Fix the include.h path ([#755](https://github.com/moq-dev/moq/pull/755))
- kixelated -> moq-dev ([#749](https://github.com/moq-dev/moq/pull/749))
- Revamp the C API and have it use hang/import ([#732](https://github.com/moq-dev/moq/pull/732))

## [0.7.0](https://github.com/moq-dev/moq/compare/libmoq-v0.6.1...libmoq-v0.7.0) - 2025-11-26

### Other

- Add initial C bindings for moq ([#722](https://github.com/kixelated/moq/pull/722))
