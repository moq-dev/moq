# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.5](https://github.com/moq-dev/moq/compare/moq-room-v0.2.4...moq-room-v0.2.5) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-auth, moq-json

## [0.2.4](https://github.com/moq-dev/moq/compare/moq-room-v0.2.3...moq-room-v0.2.4) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-json

## [0.2.3](https://github.com/moq-dev/moq/compare/moq-room-v0.2.2...moq-room-v0.2.3) - 2026-09-25

### Other

- updated the following local packages: moq-net, moq-json, moq-auth

## [0.2.2](https://github.com/moq-dev/moq/compare/moq-room-v0.2.1...moq-room-v0.2.2) - 2026-09-24

### Other

- updated the following local packages: moq-net, moq-json

## [0.2.1](https://github.com/moq-dev/moq/compare/moq-room-v0.2.0...moq-room-v0.2.1) - 2026-09-23

### Other

- updated the following local packages: moq-json, moq-tokio

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-room-v0.1.2...moq-room-v0.2.0) - 2026-09-23

### Added

- *(gateway)* [**breaking**] align embedding APIs ([#3818](https://github.com/moq-dev/moq/pull/3818))
- *(net)* [**breaking**] simplify origin scoping ([#3804](https://github.com/moq-dev/moq/pull/3804))
- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(net)* [**breaking**] announce prefixes on every wire; consumers read paths ([#3770](https://github.com/moq-dev/moq/pull/3770))
- [**breaking**] borrow publisher finish so abort can still run ([#3714](https://github.com/moq-dev/moq/pull/3714))
- *(net)* [**breaking**] one name per announce, request, and origin config concept ([#3725](https://github.com/moq-dev/moq/pull/3725))
- *(auth)* moq-auth and @moq/auth own the contract and the token ([#3684](https://github.com/moq-dev/moq/pull/3684))
- *(net)* [**breaking**] make reader group and frame limits explicit ([#3647](https://github.com/moq-dev/moq/pull/3647))

### Fixed

- *(room)* read announce events as patterns

### Other

- *(net)* [**breaking**] name path roles without new types ([#3826](https://github.com/moq-dev/moq/pull/3826))
- Merge origin/main into dev
- Merge remote-tracking branch 'origin/dev' into merge-main-into-dev-20260914
- Merge remote-tracking branch 'origin/main' into dev

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-room-v0.1.1...moq-room-v0.1.2) - 2026-09-17

### Other

- updated the following local packages: moq-net, moq-json

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-room-v0.1.0...moq-room-v0.1.1) - 2026-09-13

### Other

- updated the following local packages: kio, moq-net, moq-token, moq-json

### Added

- Announce-derived room roster, path convention, token claims, and the iroh-live chat track.
