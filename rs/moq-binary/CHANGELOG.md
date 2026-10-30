# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.18](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.17...moq-binary-v0.1.18) - 2026-10-30

### Other

- updated the following local packages: moq-net

## [0.1.17](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.16...moq-binary-v0.1.17) - 2026-10-28

### Other

- updated the following local packages: moq-net

## [0.1.16](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.15...moq-binary-v0.1.16) - 2026-10-22

### Other

- updated the following local packages: kio, moq-net

## [0.1.15](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.14...moq-binary-v0.1.15) - 2026-10-16

### Other

- updated the following local packages: moq-net

## [0.1.14](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.13...moq-binary-v0.1.14) - 2026-10-16

### Other

- updated the following local packages: moq-net

## [0.1.13](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.12...moq-binary-v0.1.13) - 2026-10-14

### Other

- updated the following local packages: moq-net

## [0.1.12](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.11...moq-binary-v0.1.12) - 2026-10-13

### Other

- updated the following local packages: moq-net

## [0.1.11](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.10...moq-binary-v0.1.11) - 2026-10-11

### Other

- updated the following local packages: moq-net

## [0.1.10](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.9...moq-binary-v0.1.10) - 2026-10-05

### Other

- updated the following local packages: moq-net

## [0.1.9](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.8...moq-binary-v0.1.9) - 2026-10-03

### Other

- updated the following local packages: moq-net

## [0.1.8](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.7...moq-binary-v0.1.8) - 2026-09-30

### Other

- updated the following local packages: kio, moq-net

## [0.1.7](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.6...moq-binary-v0.1.7) - 2026-09-27

### Added

- *(mux)* detect delay and jitter on JSON and binary tracks ([#4270](https://github.com/moq-dev/moq/pull/4270))

## [0.1.6](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.5...moq-binary-v0.1.6) - 2026-09-26

### Other

- updated the following local packages: kio, moq-net

## [0.1.5](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.4...moq-binary-v0.1.5) - 2026-09-26

### Other

- updated the following local packages: moq-net

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.3...moq-binary-v0.1.4) - 2026-09-25

### Other

- updated the following local packages: moq-net

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.2...moq-binary-v0.1.3) - 2026-09-25

### Other

- updated the following local packages: moq-net

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.1...moq-binary-v0.1.2) - 2026-09-25

### Other

- updated the following local packages: moq-flate, moq-net

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-binary-v0.1.0...moq-binary-v0.1.1) - 2026-09-24

### Other

- fill release doc gaps ([#4027](https://github.com/moq-dev/moq/pull/4027))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/moq-binary-v0.1.0) - 2026-09-23

### Added

- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(json)* [**breaking**] Config means the same thing in json and binary ([#3718](https://github.com/moq-dev/moq/pull/3718))
- *(moq-net)* [**breaking**] abort an oversized group instead of shedding its head ([#3585](https://github.com/moq-dev/moq/pull/3585))
- *(hang)* add json and binary data tracks to the catalog ([#3109](https://github.com/moq-dev/moq/pull/3109))

### Fixed

- *(moq-json)* a lost snapshot group is not fatal ([#3907](https://github.com/moq-dev/moq/pull/3907))
- *(net)* commit an unused-driven track teardown atomically ([#3455](https://github.com/moq-dev/moq/pull/3455))
- *(stream)* detect a rolled log without waiting for the group to end ([#3282](https://github.com/moq-dev/moq/pull/3282))
