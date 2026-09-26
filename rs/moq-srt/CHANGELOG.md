# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.7](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.6...moq-srt-v0.3.7) - 2026-09-26

### Added

- end a broadcast with close() in every language ([#4031](https://github.com/moq-dev/moq/pull/4031))

## [0.3.6](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.5...moq-srt-v0.3.6) - 2026-09-26

### Other

- updated the following local packages: moq-net, moq-mux

## [0.3.5](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.4...moq-srt-v0.3.5) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-mux

## [0.3.4](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.3...moq-srt-v0.3.4) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-mux

## [0.3.3](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.2...moq-srt-v0.3.3) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-mux

## [0.3.2](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.1...moq-srt-v0.3.2) - 2026-09-24

### Other

- updated the following local packages: moq-net, moq-mux

## [0.3.1](https://github.com/moq-dev/moq/compare/moq-srt-v0.3.0...moq-srt-v0.3.1) - 2026-09-23

### Other

- updated the following local packages: hang, moq-mux

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.11...moq-srt-v0.3.0) - 2026-09-23

### Added

- *(gateway)* [**breaking**] align embedding APIs ([#3818](https://github.com/moq-dev/moq/pull/3818))
- *(net)* [**breaking**] simplify origin scoping ([#3804](https://github.com/moq-dev/moq/pull/3804))
- *(hang)* [**breaking**] unify catalog APIs ([#3813](https://github.com/moq-dev/moq/pull/3813))
- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(hang)* [**breaking**] timelines only move forward ([#3711](https://github.com/moq-dev/moq/pull/3711))

### Fixed

- *(srt)* keep the listener alive until a rejected caller sees the verdict ([#3887](https://github.com/moq-dev/moq/pull/3887))

### Other

- *(rs)* read constant-size chunks with as_chunks ([#3899](https://github.com/moq-dev/moq/pull/3899))
- *(net)* [**breaking**] return the next deadline from driver polls ([#3828](https://github.com/moq-dev/moq/pull/3828))
- *(net)* [**breaking**] drive time and cache cleanup explicitly ([#3825](https://github.com/moq-dev/moq/pull/3825))
- Merge origin/main into dev
- Merge remote-tracking branch 'origin/main' into merge-main-into-dev-20260914

## [0.2.11](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.10...moq-srt-v0.2.11) - 2026-09-17

### Other

- updated the following local packages: moq-net, moq-mux, hang

## [0.2.10](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.9...moq-srt-v0.2.10) - 2026-09-13

### Other

- updated the following local packages: moq-net, hang, moq-mux

## [0.2.9](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.8...moq-srt-v0.2.9) - 2026-09-09

### Fixed

- *(moq-srt)* re-anchor egress pacing after a publisher rewind ([#3488](https://github.com/moq-dev/moq/pull/3488))

## [0.2.8](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.7...moq-srt-v0.2.8) - 2026-09-02

### Other

- updated the following local packages: moq-net, moq-mux

## [0.2.7](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.6...moq-srt-v0.2.7) - 2026-09-01

### Other

- *(rs)* point shared dependencies at [workspace.dependencies] ([#3098](https://github.com/moq-dev/moq/pull/3098))

## [0.2.6](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.5...moq-srt-v0.2.6) - 2026-08-26

### Fixed

- *(moq-srt)* preserve SCTE-35 through the SRT gateway ([#3075](https://github.com/moq-dev/moq/pull/3075))

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.4...moq-srt-v0.2.5) - 2026-08-24

### Fixed

- *(moq-cli)* pace the TS stdout export on each frame's timestamp ([#3006](https://github.com/moq-dev/moq/pull/3006))
- *(moq-srt)* preserve egress frame pacing timestamps ([#2990](https://github.com/moq-dev/moq/pull/2990))

## [0.2.4](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.3...moq-srt-v0.2.4) - 2026-08-20

### Other

- updated the following local packages: moq-mux

## [0.2.3](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.2...moq-srt-v0.2.3) - 2026-07-27

### Other

- *(moq-srt)* size the burst test below the SRT sender's drop window ([#2540](https://github.com/moq-dev/moq/pull/2540))

## [0.2.2](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.1...moq-srt-v0.2.2) - 2026-07-25

### Fixed

- *(moq-srt)* clamp egress send instants to the first packet (fixes tune-in stall) ([#2501](https://github.com/moq-dev/moq/pull/2501))

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-srt-v0.2.0...moq-srt-v0.2.1) - 2026-07-24

### Other

- updated the following local packages: moq-mux

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-srt-v0.1.2...moq-srt-v0.2.0) - 2026-07-22

### Added

- *(net)* unannounce as soon as the last route detaches ([#2419](https://github.com/moq-dev/moq/pull/2419))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- *(moq-net)* [**breaking**] model Timestamp as an instant; drop panicking arithmetic operators ([#2366](https://github.com/moq-dev/moq/pull/2366))
- Merge branch 'main' into dev
- Merge branch 'main' into dev

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-srt-v0.1.1...moq-srt-v0.1.2) - 2026-07-15

### Fixed

- *(srt)* prevent egress send buffer stalls ([#2261](https://github.com/moq-dev/moq/pull/2261))

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
