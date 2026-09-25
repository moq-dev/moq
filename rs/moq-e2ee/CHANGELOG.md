# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.3](https://github.com/moq-dev/moq/compare/moq-e2ee-v0.0.2...moq-e2ee-v0.0.3) - 2026-09-25

### Other

- updated the following local packages: moq-net

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-e2ee-v0.0.1...moq-e2ee-v0.0.2) - 2026-09-24

### Other

- updated the following local packages: moq-net

### Changed

- Profile `moq-e2ee-00`: every derivation is scoped to a publisher-minted `Epoch`, the last segment of the opaque broadcast path from `Credential::path`
- `Credential::new(credential::Config { context, kid, secret })` takes the application-owned secret; `Credential::generate`, `Pin`, and the profile argument are gone
- `Generation` replaces `Publication`: clones share per-track claims and nothing is process-global; `Name` replaces `PhysicalName`
- `track::Producer` refuses a sequence below the next allocated one before encryption; datagram plaintext is capped at `MAX_DATAGRAM_PLAINTEXT` (1160 bytes)
- Failed opens count against the key's invocation budget

### Removed

- Datagram retention and retransmission, the grouped duplicate window, the `catalog` module, and the stateless `protect`/`open`/`nonce`, HKDF, and raw key exports

## [0.0.1] - 2026-09-13

### Added

- `moq-e2ee-01` credential, physical names, track/domain keys, and AES-128-GCM protect/open
- Exclusive `Publication` and `track::Producer` / `track::Consumer` wrapping grouped-frame and datagram lifecycles
- Catalog JSON and DEFLATE-then-encrypt helpers
- Bounded duplicate windows (current plus previous group; 1024 datagram sequences)
