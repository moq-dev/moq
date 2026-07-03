# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.9](https://github.com/moq-dev/moq/compare/moq-gst-v0.2.8...moq-gst-v0.2.9) - 2026-06-30

### Other

- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))
- moqsink on a bare Element with direct (no-channel) writes ([#1893](https://github.com/moq-dev/moq/pull/1893))

## [0.2.8](https://github.com/moq-dev/moq/compare/moq-gst-v0.2.7...moq-gst-v0.2.8) - 2026-06-23

### Fixed

- *(moq-gst)* deterministic moqsrc pad names so CMAF playback works ([#1809](https://github.com/moq-dev/moq/pull/1809))

### Other

- split CLAUDE.md into per-directory guides ([#1846](https://github.com/moq-dev/moq/pull/1846))
- fix plugin license + broadcast-aligned timestamps ([#1808](https://github.com/moq-dev/moq/pull/1808))

## [0.2.7](https://github.com/moq-dev/moq/compare/moq-gst-v0.2.6...moq-gst-v0.2.7) - 2026-06-17

### Added

- *(hang)* add Catalog.Producer/Consumer wrapping @moq/json ([#1767](https://github.com/moq-dev/moq/pull/1767))

## [0.2.6](https://github.com/moq-dev/moq/compare/moq-gst-v0.2.5...moq-gst-v0.2.6) - 2026-06-16

### Other

- *(moq-gst)* moqsrc reconcile follow-ups ([#1647](https://github.com/moq-dev/moq/pull/1647)) ([#1683](https://github.com/moq-dev/moq/pull/1683))

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-gst-v0.2.4...moq-gst-v0.2.5) - 2026-06-10

### Added

- *(hang,json,moq-mux)* generic catalog with application extensions ([#1658](https://github.com/moq-dev/moq/pull/1658))

### Fixed

- *(moq-relay)* classify malformed auth-API JSON as an upstream 502
- *(moq-gst)* stop moqsrc panicking on backwards timestamps; globally unique pad ids ([#1646](https://github.com/moq-dev/moq/pull/1646))
- *(moq-gst)* follow catalog updates dynamically in moqsrc ([#1627](https://github.com/moq-dev/moq/pull/1627))

### Other

- Revert accidental commit 24d25604 (moq-native connect/reconnect refactor)
- *(moq-gst)* pump moqsrc pads directly instead of bridging to glib ([#1633](https://github.com/moq-dev/moq/pull/1633))
- add VP8 and VP9 codec support ([#1632](https://github.com/moq-dev/moq/pull/1632))
- cross-compile all x86_64-darwin release artifacts on Apple Silicon ([#1623](https://github.com/moq-dev/moq/pull/1623))
