# [XS] Text and data sections keep a prototype-named ID

## Goal

A text rendition or json/binary data track whose ID is `__proto__` is kept
by both Rust and JS, the way video and audio renditions already are. The
shared parity corpus covers each section.

## Plan

Follow-up of [moq-dev/moq#7035](https://github.com/moq-dev/moq/pull/7035),
which fixed it for video and audio only.

A malformed entry in any section refuses the whole catalog in both
languages (maintainer, 2026-10-30), which text and data sections already do
(`a_malformed_text_section_still_fails` and
`a_malformed_data_section_still_fails` in `rs/hang/src/catalog/root.rs`).
What remains is the ID: `text.ts`, `json.ts`, and `binary.ts` parse their
maps with `z.record`, which drops a `__proto__` ID that Rust keeps.

Reuse `renditions()` in `js/hang/src/catalog/section.ts`, which video and
audio use, and add text and data cases (including `__proto__`) to
`rs/hang/fixtures/catalog-parity.json`.
