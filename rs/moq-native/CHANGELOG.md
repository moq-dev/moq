# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.3](https://github.com/moq-dev/moq/compare/moq-native-v0.8.2...moq-native-v0.8.3) - 2025-09-05

### Added

- *(moq-native)* support raw QUIC sessions with `moql://` URLs ([#578](https://github.com/moq-dev/moq/pull/578))

## [0.8.2](https://github.com/moq-dev/moq/compare/moq-native-v0.8.1...moq-native-v0.8.2) - 2025-09-04

### Other

- Support aws_lc_rs or ring in moq-native ([#574](https://github.com/moq-dev/moq/pull/574))

## [0.8.0](https://github.com/moq-dev/moq/compare/moq-native-v0.7.7...moq-native-v0.8.0) - 2025-09-04

### Other

- Add WebSocket fallback support ([#570](https://github.com/moq-dev/moq/pull/570))

## [0.7.7](https://github.com/moq-dev/moq/compare/moq-native-v0.7.6...moq-native-v0.7.7) - 2025-08-12

### Other

- Less verbose errors, using % instead of ? ([#521](https://github.com/moq-dev/moq/pull/521))

## [0.7.6](https://github.com/moq-dev/moq/compare/moq-native-v0.7.5...moq-native-v0.7.6) - 2025-07-31

### Other

- updated the following local packages: moq-lite
# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.2](https://github.com/moq-dev/moq/compare/moq-native-v0.12.1...moq-native-v0.12.2) - 2026-02-09

### Other

- Revert ipv4 and fix tls.disable-verify in TOML ([#918](https://github.com/moq-dev/moq/pull/918))

## [0.12.1](https://github.com/moq-dev/moq/compare/moq-native-v0.12.0...moq-native-v0.12.1) - 2026-02-03

### Other

- Tweak a few small things the AI merge missed. ([#876](https://github.com/moq-dev/moq/pull/876))
- Remove Produce struct and simplify API ([#875](https://github.com/moq-dev/moq/pull/875))

## [0.12.0](https://github.com/moq-dev/moq/compare/moq-native-v0.11.0...moq-native-v0.12.0) - 2026-01-24

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- Add #[non_exhaustive] to moq-native configuration. ([#850](https://github.com/moq-dev/moq/pull/850))
- moq-native: Implement QUIC-LB compatible CID generation ([#848](https://github.com/moq-dev/moq/pull/848))
- Fix bugs with WebSocket fallback ([#844](https://github.com/moq-dev/moq/pull/844))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))

## [0.11.0](https://github.com/moq-dev/moq/compare/moq-native-v0.10.1...moq-native-v0.11.0) - 2026-01-10

### Added

- iroh support ([#794](https://github.com/moq-dev/moq/pull/794))

### Other

- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))
- Add debug features to moq-native ([#806](https://github.com/moq-dev/moq/pull/806))
- Certificate reloading ([#774](https://github.com/moq-dev/moq/pull/774))

## [0.10.1](https://github.com/moq-dev/moq/compare/moq-native-v0.10.0...moq-native-v0.10.1) - 2025-12-13

### Other

- kixelated -> moq-dev ([#749](https://github.com/moq-dev/moq/pull/749))
- Fix some deployment stuff. ([#747](https://github.com/moq-dev/moq/pull/747))

## [0.10.0](https://github.com/moq-dev/moq/compare/moq-native-v0.9.6...moq-native-v0.10.0) - 2025-11-26

### Other

- Upgrade web-transport ([#680](https://github.com/moq-dev/moq/pull/680))
- Add moqt:// support. ([#659](https://github.com/moq-dev/moq/pull/659))
- Allow --tls-disable-verify without false. ([#648](https://github.com/moq-dev/moq/pull/648))

## [0.9.0](https://github.com/moq-dev/moq/compare/moq-native-v0.8.4...moq-native-v0.9.0) - 2025-10-25

### Other

- Fix an arg collision with --tls-root and --cluster-root ([#637](https://github.com/moq-dev/moq/pull/637))

## [0.8.4](https://github.com/moq-dev/moq/compare/moq-native-v0.8.3...moq-native-v0.8.4) - 2025-10-18

### Other

- Fix a potential race with append_group ([#600](https://github.com/moq-dev/moq/pull/600))

## [0.7.5](https://github.com/moq-dev/moq/compare/moq-native-v0.7.4...moq-native-v0.7.5) - 2025-07-22

### Other

- Use Nix to build Docker images, supporting environment variables instead of TOML ([#486](https://github.com/moq-dev/moq/pull/486))
- Reject WebTransport connections early ([#479](https://github.com/moq-dev/moq/pull/479))

## [0.7.4](https://github.com/moq-dev/moq/compare/moq-native-v0.7.3...moq-native-v0.7.4) - 2025-07-19

### Other

- updated the following local packages: moq-lite

## [0.7.3](https://github.com/moq-dev/moq/compare/moq-native-v0.7.2...moq-native-v0.7.3) - 2025-07-16

### Other

- Remove hang-wasm and fix some minor things. ([#465](https://github.com/moq-dev/moq/pull/465))

## [0.7.2](https://github.com/moq-dev/moq/compare/moq-native-v0.7.1...moq-native-v0.7.2) - 2025-06-29

### Other

- Revamp auth one last time... for now. ([#453](https://github.com/moq-dev/moq/pull/453))

## [0.7.1](https://github.com/moq-dev/moq/compare/moq-native-v0.7.0...moq-native-v0.7.1) - 2025-06-16

### Fixed

- args for tls generate need to be without the port number ([#413](https://github.com/moq-dev/moq/pull/413))

### Other

- Default to the first certificate when SNI matching fails. ([#414](https://github.com/moq-dev/moq/pull/414))

## [0.7.0](https://github.com/moq-dev/moq/compare/moq-native-v0.6.9...moq-native-v0.7.0) - 2025-06-03

### Other

- Add support for authentication tokens ([#399](https://github.com/moq-dev/moq/pull/399))

## [0.6.9](https://github.com/moq-dev/moq/compare/moq-native-v0.6.8...moq-native-v0.6.9) - 2025-05-21

### Other

- Split into Rust/Javascript halves and rebrand as moq-lite/hang ([#376](https://github.com/moq-dev/moq/pull/376))

## [0.6.8](https://github.com/moq-dev/moq/compare/moq-native-v0.6.7...moq-native-v0.6.8) - 2025-03-09

### Other

- Less aggressive idle timeout. ([#351](https://github.com/moq-dev/moq/pull/351))

## [0.6.7](https://github.com/moq-dev/moq/compare/moq-native-v0.6.6...moq-native-v0.6.7) - 2025-03-01

### Other

- updated the following local packages: moq-transfork

## [0.6.6](https://github.com/moq-dev/moq/compare/moq-native-v0.6.5...moq-native-v0.6.6) - 2025-02-13

### Other

- Have moq-native return web_transport_quinn. ([#331](https://github.com/moq-dev/moq/pull/331))

## [0.6.5](https://github.com/moq-dev/moq/compare/moq-native-v0.6.4...moq-native-v0.6.5) - 2025-01-30

### Other

- Plane UI work ([#316](https://github.com/moq-dev/moq/pull/316))

## [0.6.4](https://github.com/moq-dev/moq/compare/moq-native-v0.6.3...moq-native-v0.6.4) - 2025-01-24

### Other

- updated the following local packages: moq-transfork

## [0.6.3](https://github.com/moq-dev/moq/compare/moq-native-v0.6.2...moq-native-v0.6.3) - 2025-01-16

### Other

- Remove the useless openssl dependency. ([#295](https://github.com/moq-dev/moq/pull/295))

## [0.6.2](https://github.com/moq-dev/moq/compare/moq-native-v0.6.1...moq-native-v0.6.2) - 2025-01-16

### Other

- Retry connections to cluster nodes ([#290](https://github.com/moq-dev/moq/pull/290))
- Switch to aws_lc_rs ([#287](https://github.com/moq-dev/moq/pull/287))
- Support fetching fingerprint via native clients. ([#286](https://github.com/moq-dev/moq/pull/286))
- Initial WASM contribute ([#283](https://github.com/moq-dev/moq/pull/283))

## [0.6.1](https://github.com/moq-dev/moq/compare/moq-native-v0.6.0...moq-native-v0.6.1) - 2025-01-13

### Other

- update Cargo.lock dependencies

## [0.6.0](https://github.com/moq-dev/moq/compare/moq-native-v0.5.10...moq-native-v0.6.0) - 2025-01-13

### Other

- Raise the keep-alive. ([#278](https://github.com/moq-dev/moq/pull/278))
- Replace mkcert with rcgen* ([#273](https://github.com/moq-dev/moq/pull/273))

## [0.5.10](https://github.com/moq-dev/moq/compare/moq-native-v0.5.9...moq-native-v0.5.10) - 2024-12-12

### Other

- Add support for RUST_LOG again. ([#267](https://github.com/moq-dev/moq/pull/267))

## [0.5.9](https://github.com/moq-dev/moq/compare/moq-native-v0.5.8...moq-native-v0.5.9) - 2024-12-04

### Other

- Move moq-gst and moq-web into the workspace. ([#258](https://github.com/moq-dev/moq/pull/258))

## [0.5.8](https://github.com/moq-dev/moq/compare/moq-native-v0.5.7...moq-native-v0.5.8) - 2024-11-26

### Other

- updated the following local packages: moq-transfork

## [0.5.7](https://github.com/moq-dev/moq/compare/moq-native-v0.5.6...moq-native-v0.5.7) - 2024-11-23

### Other

- updated the following local packages: moq-transfork

## [0.5.6](https://github.com/moq-dev/moq/compare/moq-native-v0.5.5...moq-native-v0.5.6) - 2024-11-07

### Other

- Add some more/better logging. ([#227](https://github.com/moq-dev/moq/pull/227))
- Auto upgrade dependencies with release-plz ([#224](https://github.com/moq-dev/moq/pull/224))

## [0.5.5](https://github.com/moq-dev/moq/compare/moq-native-v0.5.4...moq-native-v0.5.5) - 2024-10-29

### Other

- Karp API improvements ([#220](https://github.com/moq-dev/moq/pull/220))

## [0.5.4](https://github.com/moq-dev/moq/compare/moq-native-v0.5.3...moq-native-v0.5.4) - 2024-10-28

### Other

- updated the following local packages: moq-transfork

## [0.5.3](https://github.com/moq-dev/moq/compare/moq-native-v0.5.2...moq-native-v0.5.3) - 2024-10-27

### Other

- update Cargo.toml dependencies

## [0.5.2](https://github.com/moq-dev/moq/compare/moq-native-v0.5.1...moq-native-v0.5.2) - 2024-10-18

### Other

- updated the following local packages: moq-transfork

## [0.2.2](https://github.com/moq-dev/moq/compare/moq-native-v0.2.1...moq-native-v0.2.2) - 2024-07-24

### Other
- Add sslkeylogfile envvar for debugging ([#173](https://github.com/moq-dev/moq/pull/173))

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-native-v0.2.0...moq-native-v0.2.1) - 2024-06-03

### Other
- Revert "filter DNS query results to only include addresses that our quic endpoint can use ([#166](https://github.com/moq-dev/moq/pull/166))"
- filter DNS query results to only include addresses that our quic endpoint can use ([#166](https://github.com/moq-dev/moq/pull/166))
- Remove Cargo.lock from moq-transport
