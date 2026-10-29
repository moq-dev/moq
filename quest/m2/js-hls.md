# [M] JS HLS field

## Goal

`@moq/publish` publishes the per-rendition `hls` field and record tracks by
default, with the same records and declared duration Rust does, so a
browser-published broadcast exports as HLS like a native one.

## Plan

Decided 2026-10-27 (maintainer): records are on by default in Rust
(`moq_mux::catalog::Config` publishes them unless `with_hls(None)`), and JS
matches, until [time addressing](/quest/m1/time-addressing/consumers.md) lets
the export list segments from group starts and deletes the records. The
archive does not use the field, so the old idle `flush()` and catalog and data
track timelines stay out of this port.

Each rendition cuts its own segments on a pts grid and numbers its own
records, sharing no state with the others, as `rs/moq-mux/src/hls.rs` does.

The read side follows Catalog HLS field. Port the Rust producer
(`rs/moq-mux`) to `js/hang`, publish it from `@moq/publish` by default, and
check its records against Rust output in the interop suite.
