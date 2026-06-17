# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.19](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.18...moq-boy-v0.2.19) - 2026-06-16

### Other

- update Cargo.lock dependencies

## [0.2.18](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.17...moq-boy-v0.2.18) - 2026-06-10

### Added

- *(moq-video,moq-cli)* webcam capture and publish ([#1669](https://github.com/moq-dev/moq/pull/1669))
- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

### Fixed

- *(moq-relay)* classify malformed auth-API JSON as an upstream 502

### Other

- Revert accidental commit 24d25604 (moq-native connect/reconnect refactor)
- *(moq-native)* migrate from anyhow to thiserror ([#1651](https://github.com/moq-dev/moq/pull/1651))

## [0.2.17](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.16...moq-boy-v0.2.17) - 2026-06-03

### Other

- update Cargo.lock dependencies

## [0.2.16](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.15...moq-boy-v0.2.16) - 2026-06-02

### Other

- exit non-zero on reconnect give-up instead of hanging ([#1589](https://github.com/moq-dev/moq/pull/1589))

## [0.2.15](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.14...moq-boy-v0.2.15) - 2026-05-30

### Other

- route Android logs to logcat ([#1541](https://github.com/moq-dev/moq/pull/1541))

## [0.2.14](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.13...moq-boy-v0.2.14) - 2026-05-30

### Other

- update Cargo.lock dependencies

## [0.2.13](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.12...moq-boy-v0.2.13) - 2026-05-24

### Other

- update Cargo.lock dependencies

## [0.2.12](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.11...moq-boy-v0.2.12) - 2026-05-23

### Other

- update Cargo.lock dependencies

## [0.2.11](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.10...moq-boy-v0.2.11) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.2.10](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.9...moq-boy-v0.2.10) - 2026-05-18

### Other

- update Cargo.lock dependencies

## [0.2.9](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.8...moq-boy-v0.2.9) - 2026-05-07

### Other

- moq-mux backport + dual-API cleanup ([#1341](https://github.com/moq-dev/moq/pull/1341))
- Revert moq-lite FETCH/Subscription API changes ([#1372](https://github.com/moq-dev/moq/pull/1372))
- relocate jemalloc helper; wire it into moq-boy ([#1360](https://github.com/moq-dev/moq/pull/1360))
- backport Subscription model API for FETCH readiness ([#1348](https://github.com/moq-dev/moq/pull/1348))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.2.8](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.7...moq-boy-v0.2.8) - 2026-04-19

### Other

- Add README files for Rust crates ([#1284](https://github.com/moq-dev/moq/pull/1284))
- Clarify group delivery semantics with recv_group and next_group_ordered ([#1324](https://github.com/moq-dev/moq/pull/1324))

## [0.2.7](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.6...moq-boy-v0.2.7) - 2026-04-17

### Other

- update Cargo.lock dependencies

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.4...moq-boy-v0.2.5) - 2026-04-15

### Other

- update Cargo.lock dependencies

## [0.2.4](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.3...moq-boy-v0.2.4) - 2026-04-11

### Other

- Remove auto-reset timeout, preserve emulator state across pauses ([#1279](https://github.com/moq-dev/moq/pull/1279))

## [0.2.3](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.2...moq-boy-v0.2.3) - 2026-04-09

### Other

- Add per-component latency breakdown for moq-boy ([#1268](https://github.com/moq-dev/moq/pull/1268))
- Add automatic reconnection with exponential backoff ([#1246](https://github.com/moq-dev/moq/pull/1246))
- Reduce moq-boy input latency ([#1253](https://github.com/moq-dev/moq/pull/1253))

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-boy-v0.2.0...moq-boy-v0.2.1) - 2026-04-07

### Other

- release ([#1213](https://github.com/moq-dev/moq/pull/1213))

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-boy-v0.1.0...moq-boy-v0.2.0) - 2026-04-07

### Fixed

- *(moq-boy)* align audio and video PTS to a single wall-clock reference ([#1211](https://github.com/moq-dev/moq/pull/1211))

### Other

- dark mode, fix paths, subscription improvements ([#1226](https://github.com/moq-dev/moq/pull/1226))
- refactor Rust publisher into Session struct ([#1225](https://github.com/moq-dev/moq/pull/1225))
- Review+revamp JS player ([#1224](https://github.com/moq-dev/moq/pull/1224))
- Add location label to game viewer stats display ([#1219](https://github.com/moq-dev/moq/pull/1219))
- throttle feedback broadcast and reset on pause ([#1215](https://github.com/moq-dev/moq/pull/1215))
- Add encoding/emulation stats to moq-boy ([#1218](https://github.com/moq-dev/moq/pull/1218))
- use 4s GoP interval and 64kbps audio bitrate ([#1214](https://github.com/moq-dev/moq/pull/1214))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/moq-boy-v0.1.0) - 2026-04-03

### Other

- Set up moq-boy for publishing and add CDN infrastructure ([#1205](https://github.com/moq-dev/moq/pull/1205))
- Rename dev/ to demo/, split moq-boy into rs/ and js/ ([#1204](https://github.com/moq-dev/moq/pull/1204))
