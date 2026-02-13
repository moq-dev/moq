# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.14.0](https://github.com/moq-dev/moq/compare/hang-v0.13.1...hang-v0.14.0) - 2026-02-12

### Other

- Error cleanup ([#944](https://github.com/moq-dev/moq/pull/944))
- Reduce the moq-lite API size ([#943](https://github.com/moq-dev/moq/pull/943))

## [0.13.1](https://github.com/moq-dev/moq/compare/hang-v0.13.0...hang-v0.13.1) - 2026-02-09

### Other

- Fix video track naming to handle empty extensions ([#934](https://github.com/moq-dev/moq/pull/934))
- native client integration guide ([#931](https://github.com/moq-dev/moq/pull/931))
- Run unit tests in CI ([#921](https://github.com/moq-dev/moq/pull/921))

## [0.12.0](https://github.com/moq-dev/moq/compare/hang-v0.11.0...hang-v0.12.0) - 2026-02-03

### Other

- Rename minBuffer to jitter ([#894](https://github.com/moq-dev/moq/pull/894))
- Add support for multiple groups, and fetching them ([#877](https://github.com/moq-dev/moq/pull/877))
- Tweak a few small things the AI merge missed. ([#876](https://github.com/moq-dev/moq/pull/876))
- Remove Produce struct and simplify API ([#875](https://github.com/moq-dev/moq/pull/875))
- Close audio groups immediately. ([#870](https://github.com/moq-dev/moq/pull/870))
- CMAF passthrough attempt v3 ([#867](https://github.com/moq-dev/moq/pull/867))

## [0.11.0](https://github.com/moq-dev/moq/compare/hang-v0.10.0...hang-v0.11.0) - 2026-01-24

### Added

- *(hang)* add feature flags for third-party dependencies ([#854](https://github.com/moq-dev/moq/pull/854))

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- Add #[non_exhaustive] to moq-native configuration. ([#850](https://github.com/moq-dev/moq/pull/850))
- bump mp4-atom to 0.10.0 ([#846](https://github.com/moq-dev/moq/pull/846))
- simplify match statements using let-else syntax ([#840](https://github.com/moq-dev/moq/pull/840))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))

## [0.10.0](https://github.com/moq-dev/moq/compare/hang-v0.9.1...hang-v0.10.0) - 2026-01-10

### Added

- iroh support ([#794](https://github.com/moq-dev/moq/pull/794))

### Other

- Add generic time system with Timescale type ([#824](https://github.com/moq-dev/moq/pull/824))
- hev1 decoder ([#813](https://github.com/moq-dev/moq/pull/813))
- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))
- Add Opus decoder ([#811](https://github.com/moq-dev/moq/pull/811))

## [0.9.1](https://github.com/moq-dev/moq/compare/hang-v0.9.0...hang-v0.9.1) - 2025-12-19

### Other

- Add HLS import module ([#789](https://github.com/moq-dev/moq/pull/789))

## [0.9.0](https://github.com/moq-dev/moq/compare/hang-v0.8.0...hang-v0.9.0) - 2025-12-18

### Other

- Revert the moq_lite changes. ([#787](https://github.com/moq-dev/moq/pull/787))
- libmoq consume API ([#777](https://github.com/moq-dev/moq/pull/777))

## [0.8.0](https://github.com/moq-dev/moq/compare/hang-v0.7.0...hang-v0.8.0) - 2025-12-13

### Other

- Use BufList for hang::Frame ([#769](https://github.com/moq-dev/moq/pull/769))
- Fix and over-optimize the H.264 annex.b import ([#766](https://github.com/moq-dev/moq/pull/766))
- Add extended AAC support for variable-length AudioSpecificConfig ([#756](https://github.com/moq-dev/moq/pull/756))
- kixelated -> moq-dev ([#749](https://github.com/moq-dev/moq/pull/749))
- Revamp the C API and have it use hang/import ([#732](https://github.com/moq-dev/moq/pull/732))
- Fix some deployment stuff. ([#747](https://github.com/moq-dev/moq/pull/747))
- Revamp hang imports: consolidate annexb/cmaf into import module ([#739](https://github.com/moq-dev/moq/pull/739))
- Make a proper Timestamp type to detect overflows. ([#735](https://github.com/moq-dev/moq/pull/735))

## [0.7.0](https://github.com/moq-dev/moq/compare/hang-v0.6.1...hang-v0.7.0) - 2025-11-26

### Fixed

- wait for the first keyframe before sending frames ([#673](https://github.com/moq-dev/moq/pull/673))

### Other

- Add initial C bindings for hang ([#722](https://github.com/moq-dev/moq/pull/722))
- Add support for multiple versions ([#711](https://github.com/moq-dev/moq/pull/711))
- Upgrade web-transport ([#680](https://github.com/moq-dev/moq/pull/680))
- Remove AI features (for now) ([#664](https://github.com/moq-dev/moq/pull/664))
- hang: Handle multiple TRUN boxes within one TRAF ([#647](https://github.com/moq-dev/moq/pull/647))

## [0.6.1](https://github.com/moq-dev/moq/compare/hang-v0.6.0...hang-v0.6.1) - 2025-10-25

### Other

- updated the following local packages: moq-lite, moq-native

## [0.6.0](https://github.com/moq-dev/moq/compare/hang-v0.5.5...hang-v0.6.0) - 2025-10-18

### Added

- *(hang)* add support for annexb import ([#611](https://github.com/moq-dev/moq/pull/611))

### Other

- Use MaybeSend and MaybeSync for WASM compatibility ([#615](https://github.com/moq-dev/moq/pull/615))
- Update the catalog to better support multiple renditions. ([#616](https://github.com/moq-dev/moq/pull/616))
- Move some examples into code. ([#596](https://github.com/moq-dev/moq/pull/596))

## [0.5.5](https://github.com/moq-dev/moq/compare/hang-v0.5.4...hang-v0.5.5) - 2025-09-22

### Other

- Skip erroring groups in TrackConsumer. ([#598](https://github.com/moq-dev/moq/pull/598))

## [0.5.4](https://github.com/moq-dev/moq/compare/hang-v0.5.3...hang-v0.5.4) - 2025-09-04

### Other

- update Cargo.toml dependencies

## [0.5.3](https://github.com/moq-dev/moq/compare/hang-v0.5.2...hang-v0.5.3) - 2025-08-12

### Other

- Revamp the Producer/Consumer API for moq_lite ([#516](https://github.com/moq-dev/moq/pull/516))

## [0.5.2](https://github.com/moq-dev/moq/compare/hang-v0.5.1...hang-v0.5.2) - 2025-07-31

### Other

- Styp ([#501](https://github.com/moq-dev/moq/pull/501))

## [0.5.1](https://github.com/moq-dev/moq/compare/hang-v0.5.0...hang-v0.5.1) - 2025-07-22

### Other

- Use a size prefix for messages. ([#489](https://github.com/moq-dev/moq/pull/489))

## [0.5.0](https://github.com/moq-dev/moq/compare/hang-v0.4.1...hang-v0.5.0) - 2025-07-19

### Other

- Revamp connection URLs, broadcast paths, and origins ([#472](https://github.com/moq-dev/moq/pull/472))

## [0.4.1](https://github.com/moq-dev/moq/compare/hang-v0.4.0...hang-v0.4.1) - 2025-07-16

### Other

- Remove hang-wasm and fix some minor things. ([#465](https://github.com/moq-dev/moq/pull/465))
- Some initally AI generated documentation. ([#457](https://github.com/moq-dev/moq/pull/457))

## [0.4.0](https://github.com/moq-dev/moq/compare/hang-v0.3.0...hang-v0.4.0) - 2025-06-20

### Other

- Fix misc bugs ([#430](https://github.com/moq-dev/moq/pull/430))

## [0.3.0](https://github.com/moq-dev/moq/compare/hang-v0.2.0...hang-v0.3.0) - 2025-06-03

### Other

- Add location tracks, fix some bugs, switch to nix ([#401](https://github.com/moq-dev/moq/pull/401))
- Revamp origin/announced ([#390](https://github.com/moq-dev/moq/pull/390))
- Move config to a separate field to match the specification. ([#387](https://github.com/moq-dev/moq/pull/387))

## [0.2.0](https://github.com/moq-dev/moq/compare/hang-v0.1.0...hang-v0.2.0) - 2025-05-21

### Other

- Split into Rust/Javascript halves and rebrand as moq-lite/hang ([#376](https://github.com/moq-dev/moq/pull/376))
