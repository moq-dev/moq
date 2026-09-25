# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/moq-dev/moq/compare/moq-nvenc-v0.0.5...moq-nvenc-v0.0.6) - 2026-09-23

### Added

- [**breaking**] refuse released spellings and drop unused deprecated APIs ([#3719](https://github.com/moq-dev/moq/pull/3719))

### Fixed

- *(nvenc)* [**breaking**] make driver loading fallible ([#3838](https://github.com/moq-dev/moq/pull/3838))
- *(moq-nvenc)* roll back failed resource mapping ([#3834](https://github.com/moq-dev/moq/pull/3834))
- *(nvenc)* [**breaking**] retain resources through completion ([#3835](https://github.com/moq-dev/moq/pull/3835))

### Other

- *(quest)* complete the media release review ([#3878](https://github.com/moq-dev/moq/pull/3878))

### Changed

- [**breaking**] `Encoder::load` validates the driver table and returns a non-exhaustive
  `LoadError`, as does `Encoder::initialize_with_cuda`; the public `ENCODE_API` and `EncodeAPI`
  are removed and loading never panics.
- [**breaking**] Buffers and registrations own the encoder lifetime, `Session::encode_picture`
  consumes its buffers and returns a `Submission` that holds them through completion, and raw
  registration and configuration entry points are `unsafe`. `EncoderOutput` and the raw picture
  parameters are removed.
- Failed resource mapping unregisters the registration before releasing its owner;
  `EncodeError::cleanup` exposes a rollback failure beside the primary error.

## [0.0.5](https://github.com/moq-dev/moq/compare/moq-nvenc-v0.0.4...moq-nvenc-v0.0.5) - 2026-09-13

### Fixed

- *(moq-video)* cap NVENC keyframes with a single-frame VBV ([#3609](https://github.com/moq-dev/moq/pull/3609))

## [0.0.4](https://github.com/moq-dev/moq/compare/moq-nvenc-v0.0.3...moq-nvenc-v0.0.4) - 2026-09-01

### Other

- *(rs)* point shared dependencies at [workspace.dependencies] ([#3098](https://github.com/moq-dev/moq/pull/3098))

## [0.0.3](https://github.com/moq-dev/moq/compare/moq-nvenc-v0.0.2...moq-nvenc-v0.0.3) - 2026-07-25

### Other

- *(deps)* bump the cargo group across 1 directory with 2 updates ([#2455](https://github.com/moq-dev/moq/pull/2455))

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-nvenc-v0.0.1...moq-nvenc-v0.0.2) - 2026-07-23

### Other

- *(rust)* pin the toolchain and correct the MSRV claims ([#2462](https://github.com/moq-dev/moq/pull/2462))
