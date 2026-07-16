# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-hls-v0.1.0...moq-hls-v0.2.0) - 2026-07-15

### Fixed

- *(moq-hls)* reconcile catalog renditions ([#2266](https://github.com/moq-dev/moq/pull/2266))
- *(moq-hls)* account for audio groups in master variants ([#2264](https://github.com/moq-dev/moq/pull/2264))
- *(moq-hls)* release source subscriptions when a Broadcaster is dropped ([#2254](https://github.com/moq-dev/moq/pull/2254))

### Other

- rewrite export::Broadcaster as an owned poll-driven state machine ([#2258](https://github.com/moq-dev/moq/pull/2258))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-hls-v0.0.1) - 2026-06-30

### Other

- preserve discontinuity sequence through fMP4 import ([#1945](https://github.com/moq-dev/moq/pull/1945))
- unify rendition selection behind select::Broadcast
- [codex] Route HLS CLI import through moq-hls ([#1939](https://github.com/moq-dev/moq/pull/1939))
- [codex] Backport moq-hls to main ([#1924](https://github.com/moq-dev/moq/pull/1924))
