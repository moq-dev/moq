# [S] Archive group bounds use one inclusive range

## Goal

Archive callers pass group bounds without remembering an argument-order
exception. `Object::bounds`, range-named keys, reads, and validation use one
finite inclusive convention, with the persisted layout unchanged.

## Plan

`Object::bounds` returns `(smallest, largest)`, while `Key::groups`,
`Store::get_groups`, and `Object::{decode_groups,check_bounds}` take the
reverse order. Reuse standard Rust range notation: moq-net already accepts
`RangeBounds<u64>` in `Subscription::with_groups` and reader `set_groups`.
Archive objects require two finite bounds, so use `RangeInclusive<u64>` and
reject empty, reversed, or out-of-profile ranges at the boundary. Do not
invent another public Bounds type or expose moq-net's private normalization
helper; that helper supports unbounded subscription ranges with exclusive caps.

Keep largest-first filename serialization private to the codec. Public
enum construction must not bypass validation when a key is serialized.
An object's returned bounds should pass directly to lookup and validation.

Cover singleton and sparse ranges, reversed bounds, both identifier limits,
and direct key construction in the crate's CI tests. Preserve the exact
existing encoded paths and bytes. Update the examples and module docs inline;
this quest adds no recording or replay orchestration.

Public API: breaking range arguments and return values in moq-archive 0.0.1.
Wire and persisted format: unchanged.

## Related

- [Archive proof](/quest/m2/archive/proof.md) - storage and replay conformance beyond the API change
