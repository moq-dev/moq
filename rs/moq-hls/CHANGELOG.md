# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/moq-dev/moq/compare/moq-hls-v0.4.0...moq-hls-v0.4.1) - 2026-07-24

### Other

- updated the following local packages: moq-mux

## [0.4.0](https://github.com/moq-dev/moq/compare/moq-hls-v0.3.0...moq-hls-v0.4.0) - 2026-07-22

### Added

- *(moq-hls)* [**breaking**] expose a credential-aware serve surface for embedders ([#2438](https://github.com/moq-dev/moq/pull/2438))
- *(moq-hls)* Producer/Consumer cursors for recording a broadcast ([#2389](https://github.com/moq-dev/moq/pull/2389))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))
- *(moq-hls)* release retired renditions instead of force-closing them ([#2394](https://github.com/moq-dev/moq/pull/2394))

### Other

- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- *(mux)* [**breaking**] unseal catalog renditions and make timelines explicit/shareable ([#2420](https://github.com/moq-dev/moq/pull/2420))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- Merge main into dev
- *(moq-hls)* [**breaking**] replace the Authorizer callback with router middleware ([#2340](https://github.com/moq-dev/moq/pull/2340))
- Merge branch 'main' into dev
- Merge branch 'main' into dev

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-hls-v0.2.0...moq-hls-v0.3.0) - 2026-07-16

### Fixed

- *(moq-hls)* release track subscriptions when an export pauses ([#2298](https://github.com/moq-dev/moq/pull/2298))

### Other

- *(moq-hls)* reshape import around per-track ownership, fix HLS conformance ([#2299](https://github.com/moq-dev/moq/pull/2299))

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
