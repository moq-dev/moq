# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-rtc-v0.1.5...moq-rtc-v0.2.0) - 2026-07-22

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- *(mux)* [**breaking**] unseal catalog renditions and make timelines explicit/shareable ([#2420](https://github.com/moq-dev/moq/pull/2420))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- Merge main into dev
- *(hang)* [**breaking**] non_exhaustive catalog sections, shared container::track_info, hang draft catch-up ([#2341](https://github.com/moq-dev/moq/pull/2341))
- Merge branch 'main' into dev
- Merge branch 'main' into dev

## [0.1.5](https://github.com/moq-dev/moq/compare/moq-rtc-v0.1.4...moq-rtc-v0.1.5) - 2026-07-17

### Fixed

- *(moq-rtc)* handle IPv4-mapped peers on a dual-stack socket ([#2327](https://github.com/moq-dev/moq/pull/2327))

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-rtc-v0.1.3...moq-rtc-v0.1.4) - 2026-07-15

### Fixed

- *(moq-rtc)* synchronize RTP clocks with sender reports ([#2267](https://github.com/moq-dev/moq/pull/2267))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-rtc-v0.1.2...moq-rtc-v0.1.3) - 2026-07-12

### Other

- Add RTC H.265 and AV1 ingest bridges ([#2139](https://github.com/moq-dev/moq/pull/2139))

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-rtc-v0.1.1...moq-rtc-v0.1.2) - 2026-07-09

### Other

- Per-track timeline index for each media track ([#2109](https://github.com/moq-dev/moq/pull/2109))

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-rtc-v0.1.0...moq-rtc-v0.1.1) - 2026-07-05

### Other

- *(deps)* bump the cargo group with 9 updates ([#2098](https://github.com/moq-dev/moq/pull/2098))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-rtc-v0.0.1) - 2026-06-30

### Added

- *(moq-rtc)* add WebRTC (WHIP/WHEP) gateway ([#1916](https://github.com/moq-dev/moq/pull/1916))

### Other

- abort sessions that never receive ICE candidates ([#1951](https://github.com/moq-dev/moq/pull/1951))
- *(deps)* bump the cargo group across 1 directory with 18 updates ([#1942](https://github.com/moq-dev/moq/pull/1942))
- [codex] expose moq-rtc session runner ([#1931](https://github.com/moq-dev/moq/pull/1931))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))
