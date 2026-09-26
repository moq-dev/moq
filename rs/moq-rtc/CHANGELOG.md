# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.7](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.6...moq-rtc-v0.3.7) - 2026-09-26

### Added

- end a broadcast with close() in every language ([#4031](https://github.com/moq-dev/moq/pull/4031))

## [0.3.6](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.5...moq-rtc-v0.3.6) - 2026-09-26

### Other

- updated the following local packages: moq-net, hang, moq-mux

## [0.3.5](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.4...moq-rtc-v0.3.5) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-mux, hang

## [0.3.4](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.3...moq-rtc-v0.3.4) - 2026-09-25

### Other

- updated the following local packages: moq-net, hang, moq-mux

## [0.3.3](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.2...moq-rtc-v0.3.3) - 2026-09-25

### Added

- *(gateway)* expose the loop and handler the gateway binaries run ([#3964](https://github.com/moq-dev/moq/pull/3964))

## [0.3.2](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.1...moq-rtc-v0.3.2) - 2026-09-24

### Other

- updated the following local packages: moq-net, hang, moq-mux

## [0.3.1](https://github.com/moq-dev/moq/compare/moq-rtc-v0.3.0...moq-rtc-v0.3.1) - 2026-09-23

### Other

- updated the following local packages: moq-tokio, hang, moq-mux

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.11...moq-rtc-v0.3.0) - 2026-09-23

### Added

- *(gateway)* [**breaking**] align embedding APIs ([#3818](https://github.com/moq-dev/moq/pull/3818))
- *(net)* [**breaking**] simplify origin scoping ([#3804](https://github.com/moq-dev/moq/pull/3804))
- *(hang)* [**breaking**] unify catalog APIs ([#3813](https://github.com/moq-dev/moq/pull/3813))
- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(net)* [**breaking**] announce prefixes on every wire; consumers read paths ([#3770](https://github.com/moq-dev/moq/pull/3770))

### Other

- *(net)* [**breaking**] name path roles without new types ([#3826](https://github.com/moq-dev/moq/pull/3826))
- *(net)* [**breaking**] return the next deadline from driver polls ([#3828](https://github.com/moq-dev/moq/pull/3828))
- *(net)* [**breaking**] drive time and cache cleanup explicitly ([#3825](https://github.com/moq-dev/moq/pull/3825))
- Merge origin/main into dev

## [0.2.11](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.10...moq-rtc-v0.2.11) - 2026-09-17

### Other

- updated the following local packages: moq-net, moq-mux, hang

## [0.2.10](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.9...moq-rtc-v0.2.10) - 2026-09-13

### Other

- updated the following local packages: moq-net, hang, moq-mux

## [0.2.9](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.8...moq-rtc-v0.2.9) - 2026-09-09

### Other

- *(deps)* bump the cargo group across 1 directory with 5 updates ([#3395](https://github.com/moq-dev/moq/pull/3395))

## [0.2.8](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.7...moq-rtc-v0.2.8) - 2026-09-02

### Other

- updated the following local packages: moq-net, hang, moq-mux

## [0.2.7](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.6...moq-rtc-v0.2.7) - 2026-09-01

### Fixed

- *(rtc)* complete client media negotiation ([#3195](https://github.com/moq-dev/moq/pull/3195))

### Other

- *(rs)* point shared dependencies at [workspace.dependencies] ([#3098](https://github.com/moq-dev/moq/pull/3098))

## [0.2.6](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.5...moq-rtc-v0.2.6) - 2026-08-26

### Other

- updated the following local packages: moq-net, moq-mux, hang

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.4...moq-rtc-v0.2.5) - 2026-08-20

### Other

- *(deps)* bump the cargo group with 7 updates ([#2888](https://github.com/moq-dev/moq/pull/2888))

## [0.2.4](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.3...moq-rtc-v0.2.4) - 2026-08-14

### Fixed

- *(path)* resolve catalog references like URLs ([#2855](https://github.com/moq-dev/moq/pull/2855))
- *(rtc)* publish VP8 and VP9 dimensions ([#2845](https://github.com/moq-dev/moq/pull/2845))

## [0.2.3](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.2...moq-rtc-v0.2.3) - 2026-08-06

### Added

- *(hang)* declare a configurable 30s retention on media tracks, and fix relayed cache misses ([#2615](https://github.com/moq-dev/moq/pull/2615))

## [0.2.2](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.1...moq-rtc-v0.2.2) - 2026-07-25

### Added

- *(moq-mux)* caller-driven audio grouping via dumb importers ([#2496](https://github.com/moq-dev/moq/pull/2496))

### Fixed

- *(opus)* propagate pre-skip and encoder controls ([#2492](https://github.com/moq-dev/moq/pull/2492))

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-rtc-v0.2.0...moq-rtc-v0.2.1) - 2026-07-24

### Other

- updated the following local packages: moq-mux

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
