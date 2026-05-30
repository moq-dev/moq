# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/moq-dev/moq/compare/moq-net-v0.1.6...moq-net-v0.1.7) - 2026-05-30

### Other

- release ([#1496](https://github.com/moq-dev/moq/pull/1496))

## [0.1.6](https://github.com/moq-dev/moq/compare/moq-net-v0.1.5...moq-net-v0.1.6) - 2026-05-30

### Other

- retain entries by liveness instead of a tick retention window ([#1548](https://github.com/moq-dev/moq/pull/1548))
- auto-reconnect sessions; conducer-based Reconnect notifications ([#1544](https://github.com/moq-dev/moq/pull/1544))
- rename conducer crate to kio ([#1547](https://github.com/moq-dev/moq/pull/1547))

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-net-v0.1.3...moq-net-v0.1.4) - 2026-05-24

### Other

- *(stats)* allow multi-segment --stats-node values; move cargo-deny to ci ([#1489](https://github.com/moq-dev/moq/pull/1489))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-net-v0.1.2...moq-net-v0.1.3) - 2026-05-23

### Other

- Add stats via MoQ broadcasts ([#1442](https://github.com/moq-dev/moq/pull/1442))

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-net-v0.1.1...moq-net-v0.1.2) - 2026-05-21

### Other

- Replace mpsc with conducer for coalesced origin consumer updates ([#1433](https://github.com/moq-dev/moq/pull/1433))

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-net-v0.1.0...moq-net-v0.1.1) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.1.0] - 2026-05-18

### Added

- Initial release as `moq-net`, the networking layer that negotiates either
  the `moq-lite` or `moq-transport` wire protocol at session setup.
