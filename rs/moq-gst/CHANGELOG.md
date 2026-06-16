# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
