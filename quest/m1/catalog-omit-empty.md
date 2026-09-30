# [XS] The hang catalog omits empty video and audio sections

## Goal

A Rust-published hang catalog with no video or audio renditions leaves out the
`video` or `audio` key, as the JS publisher already does, instead of writing
`"video":{"renditions":{}},"audio":{"renditions":{}}`.

## Plan

Requested by an external consumer (OneTooMany, Discord): data-only broadcasts
fill logs with misleading empty sections.

Add `skip_serializing_if` on `video` and `audio` in
`rs/hang/src/catalog/root.rs`, like `text`/`json`/`binary`, and update the
tests that pin the empty output (`root.rs` and
`rs/moq-mux/src/catalog/hang/ext.rs`).

Wire-compatible, so it lands on `main`: Rust already defaults an absent
section on read, and `@moq/hang` has accepted an absent one since #401
(2025-06). The draft already marks both optional; decided to leave its wording
unchanged.
