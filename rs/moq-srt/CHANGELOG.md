# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-srt-v0.1.0...moq-srt-v0.1.1) - 2026-07-09

### Added

- *(moq-rtmp,moq-srt)* less aggressive default egress latency ([#2118](https://github.com/moq-dev/moq/pull/2118))

## [0.1.0](https://github.com/moq-dev/moq/compare/moq-srt-v0.0.1...moq-srt-v0.1.0) - 2026-07-04

### Added

- *(moq-cli)* per-sink frame-drop latency for the export gateways ([#1998](https://github.com/moq-dev/moq/pull/1998))

### Other

- *(release)* bump moq-rtmp/srt/rtc/hls to 0.1.0 ([#2035](https://github.com/moq-dev/moq/pull/2035))
- [codex] fix timestamp elapsed arithmetic ([#2051](https://github.com/moq-dev/moq/pull/2051))
- unified endpoint grammar (binary renamed to `moq`) ([#1985](https://github.com/moq-dev/moq/pull/1985))
- add client (dial-out) role ([#1982](https://github.com/moq-dev/moq/pull/1982))
- convert to library-only crates ([#1975](https://github.com/moq-dev/moq/pull/1975))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-srt-v0.0.1) - 2026-06-30

### Added

- *(moq-srt)* bidirectional SRT/MPEG-TS gateway (+ timestamped ts::Export) ([#1915](https://github.com/moq-dev/moq/pull/1915))

### Other

- *(deps)* bump the cargo group across 1 directory with 18 updates ([#1942](https://github.com/moq-dev/moq/pull/1942))
- [codex] Route HLS CLI import through moq-hls ([#1939](https://github.com/moq-dev/moq/pull/1939))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))
- [codex] fix moq-srt negative pacing offsets ([#1922](https://github.com/moq-dev/moq/pull/1922))
