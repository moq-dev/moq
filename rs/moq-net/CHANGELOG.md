# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.11](https://github.com/moq-dev/moq/compare/moq-net-v0.1.10...moq-net-v0.1.11) - 2026-06-16

### Fixed

- *(moq-net)* don't tear down session on unauthorized announce-interest ([#1717](https://github.com/moq-dev/moq/pull/1717))
- *(moq-net)* release cached state when a producer is aborted or dropped ([#1715](https://github.com/moq-dev/moq/pull/1715))

### Other

- rework Producer::poll/wait to a read-only predicate that returns a Mut ([#1735](https://github.com/moq-dev/moq/pull/1735))

## [0.1.10](https://github.com/moq-dev/moq/compare/moq-net-v0.1.9...moq-net-v0.1.10) - 2026-06-10

### Added

- *(moq-net)* tag broadcasts with a per-connection origin hop when the wire carries none ([#1635](https://github.com/moq-dev/moq/pull/1635))

### Fixed

- *(moq-net,js/net)* draft-18 SUBSCRIBE_NAMESPACE, subgroup headers, and announce race ([#1668](https://github.com/moq-dev/moq/pull/1668))

## [0.1.9](https://github.com/moq-dev/moq/compare/moq-net-v0.1.8...moq-net-v0.1.9) - 2026-06-03

### Other

- *(deps)* bump the cargo group (with code fixes for rand/rubato/rcgen) ([#1603](https://github.com/moq-dev/moq/pull/1603))

## [0.1.8](https://github.com/moq-dev/moq/compare/moq-net-v0.1.7...moq-net-v0.1.8) - 2026-06-01

### Other

- count connected sessions per auth root for billing ([#1574](https://github.com/moq-dev/moq/pull/1574))
- deterministic route tie-break for equal-length paths ([#1570](https://github.com/moq-dev/moq/pull/1570))
- wire session stats into the IETF protocol path ([#1560](https://github.com/moq-dev/moq/pull/1560))
- count viewers as distinct per-session subscriptions ([#1553](https://github.com/moq-dev/moq/pull/1553))

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
