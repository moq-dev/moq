# [S] Rust refuses a null catalog field as JS does

## Goal

`rs/hang` refuses a catalog in which a field the hang draft defines is
`null`, as `js/hang` already does and the draft's Root and Malformed
Renditions sections require. Today Rust reads `"bitrate": null`, or any
other optional field set to `null`, as absent, so the two languages accept
different catalogs.

## Plan

Found while landing the draft's one strict rule: that PR made every root
section and `media`'s `clock` and `join` refuse `null` in Rust
(`deserialize_some` in `rs/hang/src/catalog/mod.rs`), but the
`Option<_>` fields inside renditions, data tracks, includes, and `color`
still take serde's default, where `null` decodes as `None`. JS uses
`z.optional`, which refuses `null`.

- Pick one mechanism over annotating each field: a field-level
  `deserialize_some` breaks the `serde_as` adapters (`MillisCeil`,
  `DisplayFromStr`) those fields carry, so prefer a `serde_with` adapter
  that wraps them, or a catalog-wide check that walks only the fields the
  draft defines. An unknown extension field inside an entry may still be
  `null`.
- Add a `null` case per field kind to `rs/hang/fixtures/catalog-parity.json`
  so both languages run it.

Public API: none. Wire: a catalog with a `null` field, which JS already
refuses, is refused by Rust too.
