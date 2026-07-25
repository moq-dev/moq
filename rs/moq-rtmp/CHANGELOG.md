# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-rtmp-v0.2.0...moq-rtmp-v0.2.1) - 2026-07-24

### Other

- updated the following local packages: moq-mux

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-rtmp-v0.1.3...moq-rtmp-v0.2.0) - 2026-07-22

### Added

- *(net)* unannounce as soon as the last route detaches ([#2419](https://github.com/moq-dev/moq/pull/2419))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- Merge origin/main into dev

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-rtmp-v0.1.2...moq-rtmp-v0.1.3) - 2026-07-12

### Other

- reject unsupported RTMP FourCC plays ([#2135](https://github.com/moq-dev/moq/pull/2135))

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-rtmp-v0.1.1...moq-rtmp-v0.1.2) - 2026-07-09

### Added

- *(moq-rtmp,moq-srt)* less aggressive default egress latency ([#2118](https://github.com/moq-dev/moq/pull/2118))

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-rtmp-v0.1.0...moq-rtmp-v0.1.1) - 2026-07-05

### Other

- *(deps)* bump the cargo group with 9 updates ([#2098](https://github.com/moq-dev/moq/pull/2098))

## [0.1.0](https://github.com/moq-dev/moq/compare/moq-rtmp-v0.0.1...moq-rtmp-v0.1.0) - 2026-07-04

### Added

- *(moq-rtmp,moq-mux)* enhanced-RTMP capsEx negotiation + multitrack ([#2068](https://github.com/moq-dev/moq/pull/2068))
- *(moq-cli)* per-sink frame-drop latency for the export gateways ([#1998](https://github.com/moq-dev/moq/pull/1998))
- *(moq-mux)* add MP3 audio support for FLV/RTMP ([#1967](https://github.com/moq-dev/moq/pull/1967))

### Other

- *(release)* bump moq-rtmp/srt/rtc/hls to 0.1.0 ([#2035](https://github.com/moq-dev/moq/pull/2035))
- Enable TCP keepalive on the HTTP/WebSocket listener and RTMP client ([#2069](https://github.com/moq-dev/moq/pull/2069))
- Advertise enhanced-RTMP capabilities on connect (vendor rml_rtmp as a private module) ([#2060](https://github.com/moq-dev/moq/pull/2060))
- [codex] Fix RTMP play resolve timeout ([#2018](https://github.com/moq-dev/moq/pull/2018))
- [codex] share RTMP active publish paths ([#2019](https://github.com/moq-dev/moq/pull/2019))
- unified endpoint grammar (binary renamed to `moq`) ([#1985](https://github.com/moq-dev/moq/pull/1985))
- add client (dial-out) role ([#1982](https://github.com/moq-dev/moq/pull/1982))
- convert to library-only crates ([#1975](https://github.com/moq-dev/moq/pull/1975))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-rtmp-v0.0.1) - 2026-06-30

### Added

- *(moq-rtmp)* RTMP/E-RTMP gateway + enhanced-RTMP FLV codecs on main ([#1914](https://github.com/moq-dev/moq/pull/1914))

### Other

- *(deps)* bump the cargo group across 1 directory with 18 updates ([#1942](https://github.com/moq-dev/moq/pull/1942))
- [codex] Route HLS CLI import through moq-hls ([#1939](https://github.com/moq-dev/moq/pull/1939))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))
