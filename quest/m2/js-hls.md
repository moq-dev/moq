# [M] JS HLS field

## Goal

`@moq/publish` publishes the opt-in per-rendition `hls` field and tracks Rust
does, with the same records and declared duration, when the application
enables it, so a browser-published broadcast exports as HLS like a native one.
Nothing publishes it by default.

## Plan

Retargeted 2026-10-17: the field exists only for HLS compatibility and is
opt-in (Catalog HLS field, landed). The archive
does not use it, so the old idle `flush()` and catalog and data track timelines
are dropped from this port.

Each rendition cuts its own segments on a pts grid and numbers its own
records, sharing no state with the others, as `rs/moq-mux/src/hls.rs` does.

The read side follows Catalog HLS field. Port the Rust producer
(`rs/moq-mux`) to `js/hang`, wire the opt-in through `@moq/publish`, and check
its records against Rust output in the interop suite.

