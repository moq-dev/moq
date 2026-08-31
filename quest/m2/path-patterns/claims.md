# [L] Claims

## Goal

Rust and TypeScript token libraries verify v0 prefix claims forever and add an
exact v1 wire shape:

```json
{"v":1,"root":"pid","publish":["*/chat"],"subscribe":["**/*.hang"]}
```

## Plan

- Decode claims as a discriminated v0/v1 union. Missing `v` reads legacy
  `put`/`get`; v1 reads `publish`/`subscribe`; unknown versions or mixed fields
  fail closed. Lists normalize to reduced unions.
- Make `authorize` return exact residual pattern sets in both root directions.
  Scope containment accepts every valid subset and rejects every escape.
- Version the JWK `scope` object the same way. An unscoped existing key may
  sign v1. A legacy scoped key may sign only v0; v1 requires a newly issued v1
  scope. Enforce the ceiling on signing and verification.
- Preserve the current public Rust field vocabulary where practical, but do
  not create ambiguous structs whose serialization changes by accident. Use
  explicit v0/v1 types and conversions.
- Add Rust/TypeScript signing, verification, authorization, scope, and
  cross-version interoperability vectors, including old-reader fail-closed
  behavior.

## Required

- [Matcher](/quest/m2/path-patterns/matcher.md)
