# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- The H.264 decoder releases a picture as soon as the SPS's reorder limit allows, as the H.265 decoder already did, instead of holding it until a later picture needs its frame buffer. A stream without reordering, such as openh264's, comes out as it decodes rather than one access unit late. A stream that reorders more than its VUI declares raises the limit for itself, up to the DPB size.

### Changed

- Moved into [moq-dev/moq](https://github.com/moq-dev/moq) from [moq-dev/vaapi](https://github.com/moq-dev/vaapi), whose history covers 0.2.1 and earlier.
- Logs through `tracing` instead of `log`.
