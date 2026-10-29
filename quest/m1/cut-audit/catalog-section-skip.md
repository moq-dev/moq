# [S] Every catalog section skips a malformed entry

## Goal

A malformed text rendition or json/binary data track is skipped with a
warning in both Rust and JS, keeping the rest of the catalog, the way video
and audio renditions already are. A text or data entry whose ID is
`__proto__` is kept by both languages. The shared parity corpus covers each
section.

## Plan

Follow-up of [moq-dev/moq#7035](https://github.com/moq-dev/moq/pull/7035),
which applied per-rendition skipping to video and audio only. The maintainer
picked one rule everywhere when settling that PR's decisions.

Today a malformed text, json, or binary entry refuses the whole catalog
(`a_malformed_text_section_still_fails` and
`a_malformed_data_section_still_fails` in `rs/hang/src/catalog/root.rs`).
In JS, `text.ts`, `json.ts`, and `binary.ts` parse their maps with
`z.record`, which drops a `__proto__` ID that Rust keeps.

Reuse what video and audio use: `deserialize_renditions` in
`rs/hang/src/catalog/mod.rs` and `renditions()` in
`js/hang/src/catalog/section.ts`. Flip the two Rust tests to expect a skip,
add text and data cases (including `__proto__`) to
`rs/hang/fixtures/catalog-parity.json`, and extend the draft's "Malformed
Renditions" section to name every section.
