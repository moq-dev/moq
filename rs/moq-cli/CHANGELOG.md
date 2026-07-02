# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.35](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.34...moq-cli-v0.7.35) - 2026-06-30

### Added

- *(moq-srt)* bidirectional SRT/MPEG-TS gateway (+ timestamped ts::Export) ([#1915](https://github.com/moq-dev/moq/pull/1915))
- *(hang)* compressed catalog track (catalog.json.z) ([#1904](https://github.com/moq-dev/moq/pull/1904))

### Other

- *(deps)* bump the cargo group across 1 directory with 18 updates ([#1942](https://github.com/moq-dev/moq/pull/1942))
- [codex] Route HLS CLI import through moq-hls ([#1939](https://github.com/moq-dev/moq/pull/1939))
- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

## [0.7.34](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.33...moq-cli-v0.7.34) - 2026-06-23

### Added

- *(catalog)* expose untyped catalog extensions via moq-ffi and libmoq ([#1886](https://github.com/moq-dev/moq/pull/1886))
- *(moq-cli)* wire verbatim MPEG-TS carriage through publish/subscribe ([#1842](https://github.com/moq-dev/moq/pull/1842))

### Other

- move moq-cli's TS verbatim coverage into moq-mux ([#1879](https://github.com/moq-dev/moq/pull/1879))

## [0.7.33](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.32...moq-cli-v0.7.33) - 2026-06-19

### Other

- update Cargo.lock dependencies

## [0.7.32](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.31...moq-cli-v0.7.32) - 2026-06-16

### Other

- Add FLV (Flash Video / RTMP) container support to moq-mux ([#1745](https://github.com/moq-dev/moq/pull/1745))
- Windows support: dual-stack IPv4/IPv6 sockets, setup.bat, and `just dev` ([#1732](https://github.com/moq-dev/moq/pull/1732))
- *(moq-cli)* remove the capture feature ([#1728](https://github.com/moq-dev/moq/pull/1728))

## [0.7.31](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.30...moq-cli-v0.7.31) - 2026-06-10

### Added

- *(moq-video,moq-cli)* webcam capture and publish ([#1669](https://github.com/moq-dev/moq/pull/1669))
- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

### Fixed

- *(moq-relay)* classify malformed auth-API JSON as an upstream 502

### Other

- Revert accidental commit 24d25604 (moq-native connect/reconnect refactor)
- *(moq-native)* migrate from anyhow to thiserror ([#1651](https://github.com/moq-dev/moq/pull/1651))

## [0.7.30](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.29...moq-cli-v0.7.30) - 2026-06-03

### Other

- add MPEG-TS (transport stream) import and export ([#1587](https://github.com/moq-dev/moq/pull/1587))

## [0.7.29](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.28...moq-cli-v0.7.29) - 2026-06-02

### Other

- update Cargo.lock dependencies

## [0.7.28](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.27...moq-cli-v0.7.28) - 2026-05-30

### Other

- route Android logs to logcat ([#1541](https://github.com/moq-dev/moq/pull/1541))

## [0.7.27](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.26...moq-cli-v0.7.27) - 2026-05-30

### Other

- update Cargo.lock dependencies

## [0.7.26](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.25...moq-cli-v0.7.26) - 2026-05-24

### Other

- update Cargo.lock dependencies

## [0.7.25](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.24...moq-cli-v0.7.25) - 2026-05-23

### Added

- Unified CMSF/Hang pipeline (cleanup of #1429) ([#1444](https://github.com/moq-dev/moq/pull/1444))

### Other

- Add Matroska/WebM import and export support ([#1438](https://github.com/moq-dev/moq/pull/1438))
- Auto-detect catalog format from broadcast name extension ([#1394](https://github.com/moq-dev/moq/pull/1394))

## [0.7.24](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.23...moq-cli-v0.7.24) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.7.23](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.22...moq-cli-v0.7.23) - 2026-05-18

### Other

- update Cargo.lock dependencies

## [0.7.21](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.20...moq-cli-v0.7.21) - 2026-05-07

### Other

- moq-mux backport + dual-API cleanup ([#1341](https://github.com/moq-dev/moq/pull/1341))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.7.20](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.19...moq-cli-v0.7.20) - 2026-04-20

### Other

- update Cargo.lock dependencies

## [0.7.19](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.18...moq-cli-v0.7.19) - 2026-04-19

### Other

- resolve DNS hostnames in --server-bind ([#1332](https://github.com/moq-dev/moq/pull/1332))

## [0.7.18](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.17...moq-cli-v0.7.18) - 2026-04-17

### Other

- update Cargo.lock dependencies

## [0.7.17](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.16...moq-cli-v0.7.17) - 2026-04-15

### Other

- Refactor publish stats logging to use structured logging ([#1290](https://github.com/moq-dev/moq/pull/1290))

## [0.7.16](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.15...moq-cli-v0.7.16) - 2026-04-11

### Other

- update Cargo.lock dependencies

## [0.7.15](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.14...moq-cli-v0.7.15) - 2026-04-09

### Other

- Clean up CLI publish stats output ([#1263](https://github.com/moq-dev/moq/pull/1263))
- Add import statistics tracking and reporting ([#1242](https://github.com/moq-dev/moq/pull/1242))
- Add automatic reconnection with exponential backoff ([#1246](https://github.com/moq-dev/moq/pull/1246))

## [0.7.14](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.13...moq-cli-v0.7.14) - 2026-04-07

### Other

- Switch Docker images from kixelated/ to moqdev/ ([#1234](https://github.com/moq-dev/moq/pull/1234))

## [0.7.13](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.12...moq-cli-v0.7.13) - 2026-03-31

### Other

- update Cargo.lock dependencies

## [0.7.12](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.11...moq-cli-v0.7.12) - 2026-03-25

### Other

- update Cargo.lock dependencies

## [0.7.11](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.10...moq-cli-v0.7.11) - 2026-03-16

### Other

- update Cargo.lock dependencies

## [0.7.10](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.9...moq-cli-v0.7.10) - 2026-03-13

### Other

- Uniffi async objects ([#1071](https://github.com/moq-dev/moq/pull/1071))
- Set MSRV to 1.85 (edition 2024) ([#1083](https://github.com/moq-dev/moq/pull/1083))

## [0.7.8](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.7...moq-cli-v0.7.8) - 2026-03-03

### Other

- Rename `moq` binary to `moq-cli` ([#1023](https://github.com/moq-dev/moq/pull/1023))
- Add moq-msf crate for MSF catalog support ([#993](https://github.com/moq-dev/moq/pull/993))
- Replace tokio::sync::watch with custom Producer/Subscriber ([#996](https://github.com/moq-dev/moq/pull/996))

## [0.7.7](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.6...moq-cli-v0.7.7) - 2026-02-12

### Other

- (AI) Add support for quiche to moq-native ([#928](https://github.com/moq-dev/moq/pull/928))

## [0.7.6](https://github.com/moq-dev/moq/compare/moq-cli-v0.7.5...moq-cli-v0.7.6) - 2026-02-09

### Other

- update Cargo.lock dependencies

## [0.7.5](https://github.com/moq-dev/moq/compare/hang-cli-v0.7.4...hang-cli-v0.7.5) - 2026-02-03

### Other

- Remove Produce struct and simplify API ([#875](https://github.com/moq-dev/moq/pull/875))
- CMAF passthrough attempt v3 ([#867](https://github.com/moq-dev/moq/pull/867))

## [0.7.4](https://github.com/moq-dev/moq/compare/hang-cli-v0.7.3...hang-cli-v0.7.4) - 2026-01-24

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))

## [0.7.3](https://github.com/moq-dev/moq/compare/hang-cli-v0.7.2...hang-cli-v0.7.3) - 2026-01-10

### Added

- iroh support ([#794](https://github.com/moq-dev/moq/pull/794))

### Other

- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))
- Include sd-notify only on unix ([#807](https://github.com/moq-dev/moq/pull/807))
- Fix a rustls panic causing the HTTPS server to not work. ([#804](https://github.com/moq-dev/moq/pull/804))
- Certificate reloading ([#774](https://github.com/moq-dev/moq/pull/774))

## [0.7.2](https://github.com/moq-dev/moq/compare/hang-cli-v0.7.1...hang-cli-v0.7.2) - 2025-12-19

### Other

- Add HLS import module ([#789](https://github.com/moq-dev/moq/pull/789))

## [0.7.1](https://github.com/moq-dev/moq/compare/hang-cli-v0.7.0...hang-cli-v0.7.1) - 2025-12-18

### Other

- updated the following local packages: hang

## [0.7.0](https://github.com/moq-dev/moq/compare/hang-cli-v0.6.1...hang-cli-v0.7.0) - 2025-11-26

### Other

- update Cargo.lock dependencies

## [0.6.0](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.11...hang-cli-v0.6.0) - 2025-10-25

### Other

- Fix an arg collision with --tls-root and --cluster-root ([#637](https://github.com/moq-dev/moq/pull/637))
- Add systemd notify support ([#634](https://github.com/moq-dev/moq/pull/634))

## [0.2.11](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.10...hang-cli-v0.2.11) - 2025-10-18

### Added

- *(hang)* add support for annexb import ([#611](https://github.com/moq-dev/moq/pull/611))

### Other

- Use MaybeSend and MaybeSync for WASM compatibility ([#615](https://github.com/moq-dev/moq/pull/615))

## [0.2.10](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.9...hang-cli-v0.2.10) - 2025-09-05

### Added

- *(moq-native)* support raw QUIC sessions with `moql://` URLs ([#578](https://github.com/moq-dev/moq/pull/578))

## [0.2.9](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.8...hang-cli-v0.2.9) - 2025-09-04

### Other

- update Cargo.lock dependencies

## [0.2.7](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.6...hang-cli-v0.2.7) - 2025-09-04

### Other

- Add WebSocket fallback support ([#570](https://github.com/moq-dev/moq/pull/570))

## [0.2.6](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.5...hang-cli-v0.2.6) - 2025-08-21

### Other

- update Cargo.lock dependencies

## [0.2.5](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.4...hang-cli-v0.2.5) - 2025-08-12

### Other

- Support an array of authorized paths ([#536](https://github.com/moq-dev/moq/pull/536))
- Revamp the Producer/Consumer API for moq_lite ([#516](https://github.com/moq-dev/moq/pull/516))
- Less verbose errors, using % instead of ? ([#521](https://github.com/moq-dev/moq/pull/521))

## [0.2.4](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.3...hang-cli-v0.2.4) - 2025-07-31

### Other

- update Cargo.lock dependencies

## [0.2.1](https://github.com/moq-dev/moq/compare/hang-cli-v0.2.0...hang-cli-v0.2.1) - 2025-07-22

### Other

- Add an ANNOUNCE_INIT message. ([#483](https://github.com/moq-dev/moq/pull/483))
- Reject WebTransport connections early ([#479](https://github.com/moq-dev/moq/pull/479))
- The root shouldn't announce itself. ([#473](https://github.com/moq-dev/moq/pull/473))

## [0.1.10](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.9...hang-cli-v0.1.10) - 2025-07-19

### Other

- Revamp connection URLs, broadcast paths, and origins ([#472](https://github.com/moq-dev/moq/pull/472))

## [0.1.9](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.8...hang-cli-v0.1.9) - 2025-07-16

### Other

- update Cargo.lock dependencies

## [0.1.8](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.7...hang-cli-v0.1.8) - 2025-06-29

### Other

- update Cargo.lock dependencies

## [0.1.7](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.6...hang-cli-v0.1.7) - 2025-06-25

### Other

- update Cargo.lock dependencies

## [0.1.6](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.5...hang-cli-v0.1.6) - 2025-06-20

### Other

- update Cargo.lock dependencies

## [0.1.2](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.1...hang-cli-v0.1.2) - 2025-06-16

### Other

- update Cargo.lock dependencies

## [0.1.1](https://github.com/moq-dev/moq/compare/hang-cli-v0.1.0...hang-cli-v0.1.1) - 2025-06-03

### Other

- Add support for authentication tokens ([#399](https://github.com/moq-dev/moq/pull/399))
- Add location tracks, fix some bugs, switch to nix ([#401](https://github.com/moq-dev/moq/pull/401))
- Revamp origin/announced ([#390](https://github.com/moq-dev/moq/pull/390))
- Move config to a separate field to match the specification. ([#387](https://github.com/moq-dev/moq/pull/387))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/hang-cli-v0.1.0) - 2025-05-21

### Other

- Split into Rust/Javascript halves and rebrand as moq-lite/hang ([#376](https://github.com/moq-dev/moq/pull/376))
