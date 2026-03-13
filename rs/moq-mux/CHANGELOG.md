# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/moq-dev/moq/compare/moq-mux-v0.3.1...moq-mux-v0.3.2) - 2026-03-13

### Other

- Uniffi async objects ([#1071](https://github.com/moq-dev/moq/pull/1071))
- Set MSRV to 1.85 (edition 2024) ([#1083](https://github.com/moq-dev/moq/pull/1083))

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.2.1...moq-mux-v0.3.0) - 2026-03-03

### Fixed

- mask AAC profile to 5 bits to prevent shift overflow ([#1028](https://github.com/moq-dev/moq/pull/1028))
- `Fmp4::init_audio()` doesn't populate description for AAC, causing downstream format mismatch ([#1024](https://github.com/moq-dev/moq/pull/1024))

### Other

- OrderedProducer API with max_group_duration ([#1007](https://github.com/moq-dev/moq/pull/1007))
- Tweak the API to revert some breaking changes. ([#1036](https://github.com/moq-dev/moq/pull/1036))
- Add typed initialization for Opus and AAC in moq-mux ([#1034](https://github.com/moq-dev/moq/pull/1034))
- Cache and re-insert parameter sets before keyframes in H.264/H.265 ([#1030](https://github.com/moq-dev/moq/pull/1030))
- Add moq-msf crate for MSF catalog support ([#993](https://github.com/moq-dev/moq/pull/993))
- Replace tokio::sync::watch with custom Producer/Subscriber ([#996](https://github.com/moq-dev/moq/pull/996))

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-mux-v0.2.0...moq-mux-v0.2.1) - 2026-02-12

### Other

- (AI) Add support for quiche to moq-native ([#928](https://github.com/moq-dev/moq/pull/928))

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-mux-v0.1.0...moq-mux-v0.2.0) - 2026-02-09

### Other

- AV1 decoder ([#920](https://github.com/moq-dev/moq/pull/920))
- Split Decoder into Decoder and StreamDecoder variants. ([#912](https://github.com/moq-dev/moq/pull/912))
- Use `moq` instead of `hang` for some crates ([#906](https://github.com/moq-dev/moq/pull/906))
