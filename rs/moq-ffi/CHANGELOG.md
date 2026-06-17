# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.21](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.20...moq-ffi-v0.2.21) - 2026-06-16

### Added

- certificate pinning for native and browser clients ([#1698](https://github.com/moq-dev/moq/pull/1698))
- *(moq-ffi)* expose dynamic track requests ([#1674](https://github.com/moq-dev/moq/pull/1674))

### Fixed

- *(native)* surface terminal auth connect errors ([#1649](https://github.com/moq-dev/moq/pull/1649))

### Other

- Mux import with existing track ([#1684](https://github.com/moq-dev/moq/pull/1684))

## [0.2.20](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.19...moq-ffi-v0.2.20) - 2026-06-10

### Added

- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

## [0.2.19](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.18...moq-ffi-v0.2.19) - 2026-06-03

### Other

- update Cargo.lock dependencies

## [0.2.18](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.17...moq-ffi-v0.2.18) - 2026-06-02

### Other

- shrink moq-ffi & libmoq staticlibs with LTO (unblocks the moq-go mirror push) ([#1577](https://github.com/moq-dev/moq/pull/1577))

## [0.2.17](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.16...moq-ffi-v0.2.17) - 2026-05-30

### Other

- route Android logs to logcat ([#1541](https://github.com/moq-dev/moq/pull/1541))

## [0.2.16](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.15...moq-ffi-v0.2.16) - 2026-05-30

### Other

- ship moq.h and linux staticlibs so the Go module builds for consumers ([#1549](https://github.com/moq-dev/moq/pull/1549))
- streaming media import + cross-language interop smoke test ([#1529](https://github.com/moq-dev/moq/pull/1529))
- re-export FFI, session.shutdown(); explicit Origin wiring ([#1526](https://github.com/moq-dev/moq/pull/1526))

## [0.2.15](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.14...moq-ffi-v0.2.15) - 2026-05-27

### Other

- moq-mux: add seek(sequence) on importers for explicit group boundaries ([#1515](https://github.com/moq-dev/moq/pull/1515))
- moq-net: add Lite05Wip version variant (unadvertised) ([#1518](https://github.com/moq-dev/moq/pull/1518))

## [0.2.14](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.13...moq-ffi-v0.2.14) - 2026-05-25

### Other

- ci(swift): decouple release manifest from dev Package.swift and gate publish on SPM resolve ([#1502](https://github.com/moq-dev/moq/pull/1502))

## [0.2.13](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.12...moq-ffi-v0.2.13) - 2026-05-24

### Added

- add moq-audio crate, raw-audio FFI, and rename moq-codec to moq-video ([#1484](https://github.com/moq-dev/moq/pull/1484))

## [0.2.12](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.11...moq-ffi-v0.2.12) - 2026-05-23

### Other

- Add Python MoQ server API with session acceptance and handshake ([#1417](https://github.com/moq-dev/moq/pull/1417))
- Tighten moq-ffi release pipeline ahead of first publish ([#1447](https://github.com/moq-dev/moq/pull/1447))
- Add Low Overhead Container (LOC) frame format support ([#1388](https://github.com/moq-dev/moq/pull/1388))
- re-emit deprecated CMAF timescale/trackId in catalog ([#1440](https://github.com/moq-dev/moq/pull/1440))
- Add Swift and Kotlin FFI wrappers with packaging and publishing ([#1432](https://github.com/moq-dev/moq/pull/1432))

## [0.2.11](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.10...moq-ffi-v0.2.11) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.2.10](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.9...moq-ffi-v0.2.10) - 2026-05-18

### Other

- Expose track name and used/unused activity signals ([#1398](https://github.com/moq-dev/moq/pull/1398))

## [0.2.8](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.7...moq-ffi-v0.2.8) - 2026-05-07

### Other

- moq-mux backport + dual-API cleanup ([#1341](https://github.com/moq-dev/moq/pull/1341))
- tighten public API surface and remove deprecated methods ([#1378](https://github.com/moq-dev/moq/pull/1378))
- Revert moq-lite FETCH/Subscription API changes ([#1372](https://github.com/moq-dev/moq/pull/1372))
- backport Subscription model API for FETCH readiness ([#1348](https://github.com/moq-dev/moq/pull/1348))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.2.7](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.6...moq-ffi-v0.2.7) - 2026-04-19

### Other

- Adding  data (aka json) to the py_lib ([#1318](https://github.com/moq-dev/moq/pull/1318))

## [0.2.6](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.5...moq-ffi-v0.2.6) - 2026-04-17

### Other

- update Cargo.lock dependencies

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.4...moq-ffi-v0.2.5) - 2026-04-15

### Other

- update Cargo.lock dependencies

## [0.2.4](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.3...moq-ffi-v0.2.4) - 2026-04-11

### Other

- update Cargo.lock dependencies

## [0.2.3](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.2...moq-ffi-v0.2.3) - 2026-04-09

### Other

- Add bandwidth estimation for adaptive bitrate control ([#1208](https://github.com/moq-dev/moq/pull/1208))

## [0.2.2](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.1...moq-ffi-v0.2.2) - 2026-04-07

### Other

- update Cargo.lock dependencies

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-ffi-v0.2.0...moq-ffi-v0.2.1) - 2026-04-03

### Other

- update Cargo.lock dependencies

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.6...moq-ffi-v0.2.0) - 2026-03-26

### Other

- Use typed ordered::Consumer for video/audio in moq-ffi ([#1163](https://github.com/moq-dev/moq/pull/1163))

## [0.1.6](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.5...moq-ffi-v0.1.6) - 2026-03-25

### Other

- update Cargo.lock dependencies

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.3...moq-ffi-v0.1.4) - 2026-03-18

### Other

- Fix FFI test panic strategy mismatch ([#1128](https://github.com/moq-dev/moq/pull/1128))
- Remove unused dev-dependencies and bump @moq/qmux ([#1126](https://github.com/moq-dev/moq/pull/1126))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-ffi-v0.1.2...moq-ffi-v0.1.3) - 2026-03-16

### Other

- Add FFI test for objects without tokio runtime ([#1112](https://github.com/moq-dev/moq/pull/1112))
- Fix MoqSession drop requiring tokio runtime ([#1109](https://github.com/moq-dev/moq/pull/1109))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/moq-ffi-v0.1.0) - 2026-03-13

### Other

- Publish moq-ffi just to trigger release-plz. ([#1094](https://github.com/moq-dev/moq/pull/1094))
- Uniffi async objects ([#1071](https://github.com/moq-dev/moq/pull/1071))
