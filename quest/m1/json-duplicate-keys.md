# [S] JSON decoders refuse duplicate keys in Rust and JS

## Goal

Every `moq-json` and `@moq/json` decoder (snapshot, delta, stream, and
window) refuses an object with a duplicate key, so a catalog or data frame
can never mean two things to two readers.

## Plan

Today Rust refuses duplicates in deltas only; snapshots go through
`serde_json` into `Value`, which keeps the last key. JS uses `JSON.parse`
everywhere, which also keeps the last key. #5806 adds a typed refusal for a
duplicate catalog `version` only.

Decided 2026-10-17 (maintainer): refuse everywhere, in both languages. For
JS, prefer a maintained parser over hand-rolling: `@humanwhocodes/momoa`
(maintained, widely used) keeps duplicate members in its AST, so walk it;
`json-dup-key-validator` is unmaintained since 2021. Measure the parse cost
against `JSON.parse` on catalog-sized and stats-sized frames, since every
reader pays it.

Public API: decoders return a new error on duplicate keys. Wire: frames
with duplicate keys, previously accepted, are refused.

## Related

- #5806 - adds the duplicate `version` refusal
