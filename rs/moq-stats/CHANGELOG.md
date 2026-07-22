# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/moq-stats-v0.1.0) - 2026-07-22

### Added

- *(net)* unannounce as soon as the last route detaches ([#2419](https://github.com/moq-dev/moq/pull/2419))
- *(net)* [**breaking**] extract stats publishing into moq-stats with compressed tracks ([#2380](https://github.com/moq-dev/moq/pull/2380))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))

### Other

- *(stats)* [**breaking**] collect traffic counters in the model layer ([#2427](https://github.com/moq-dev/moq/pull/2427))
- [**breaking**] pre-bump API polish across the release batch ([#2423](https://github.com/moq-dev/moq/pull/2423))
- compile doc examples across the workspace ([#2421](https://github.com/moq-dev/moq/pull/2421))
- *(stats)* [**breaking**] remove internal tier defaults ([#2411](https://github.com/moq-dev/moq/pull/2411))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
