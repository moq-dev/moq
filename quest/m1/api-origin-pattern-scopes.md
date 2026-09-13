# [M] Origin scope pattern API

## Goal

Published origin scope APIs use the shared pattern-union types before the
release, while preserving every currently supported prefix grant. General
pattern enforcement remains in m2 without another signature change.

## Plan

Replace prefix-valued public scope inputs and allowed-scope outputs in Rust
and JavaScript with the shared pattern-union types. Include public selection
operations that expose `PathPrefixes`. Roots remain literal paths. Keep the
existing refusal result for scopes that cannot be represented; do not add a
second scope method or silently narrow or widen a grant.

Implement prefix-shaped unions completely: `foo/**` retains the old `foo`
prefix meaning and `**` retains the old empty-prefix meaning. An empty union
grants nothing. An exact `foo` or empty pattern is not a prefix grant and must
be refused until general pattern enforcement lands. Validate the whole union
before constructing a scoped handle, so a supported member cannot conceal an
unsupported one.

Convert every existing caller's intended prefixes explicitly, including relay
auth, cluster sessions, HLS, stats, native clients, examples, and any binding
that exposes these operations. `allowed` reports the actual grant in the new
vocabulary. Preserve nested scoping and literal-root rebasing for all accepted
unions. Keep legacy token and wire prefix decoding unchanged at their boundary.

Run Rust/JS scope tests and the existing cross-language CI cases. Cover the
root grant, empty union, multiple prefixes, nested roots, unchanged allowed
and refused paths, and explicit refusal of exact, suffix, and segment-wildcard
grants. Update public API documentation. No new wire encoding or v1 token
default belongs here. Target dev.

## Related

- [Origin scopes](/quest/m2/path-patterns/origin.md) - implements the full pattern algebra through this API
