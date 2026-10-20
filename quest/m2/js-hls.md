# [M] JS HLS field

## Goal

`@moq/publish` publishes the opt-in per-rendition `hls` field and tracks Rust
does, with the same records and declared duration, when the application
enables it, so a browser-published broadcast exports as HLS like a native one.
Nothing publishes it by default.

## Plan

Retargeted 2026-10-17: the field exists only for HLS compatibility and is
opt-in ([Catalog HLS field](/quest/m1/archive/catalog-hls.md)). The archive
does not use it, so the old idle `flush()` and catalog and data track timelines
are dropped from this port.

Enabling it carries the aligned-segment contract: the publisher starts groups
at the same pts across the renditions it lists, so segment n starts at the same
time in each. Check the encoders can honor it and refuse to enable it where
they cannot.

The read side follows Catalog HLS field. Port the Rust producer
(`rs/moq-mux`) to `js/hang`, wire the opt-in through `@moq/publish`, and check
its records against Rust output in the interop suite.

## Required

- [Catalog HLS field](/quest/m1/archive/catalog-hls.md) - the field shape this ports
