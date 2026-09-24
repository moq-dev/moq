# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
