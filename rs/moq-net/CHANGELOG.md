# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-net-v0.1.0...moq-net-v0.1.1) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.1.0] - 2026-05-18

### Added

- Initial release as `moq-net`.

This crate was previously published as [`moq-lite`](https://crates.io/crates/moq-lite).
The new name reflects that it is the networking layer; under the hood it negotiates
either the `moq-lite` or `moq-transport` wire protocol at session setup. For history
prior to the rename, see the
[`moq-lite` changelog](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/CHANGELOG.md).
