# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
