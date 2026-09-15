# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.1] - 2026-09-13

### Added

- `moq-e2ee-01` credential, physical names, track/domain keys, and AES-128-GCM protect/open
- Exclusive `Publication` and `track::Producer` / `track::Consumer` wrapping grouped-frame and datagram lifecycles
- Catalog JSON and DEFLATE-then-encrypt helpers
- Bounded duplicate windows (current plus previous group; 1024 datagram sequences)
