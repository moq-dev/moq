# [S] Narrow the model before it goes public

## Goal

Every model-internal Rust item in `rs/moq-net/src/model/` is
`pub(in crate::model)` (or `pub(super)`), so a `pub(crate)` left there is
either marked deliberately private (passthrough, tuning, capability gap,
test-only) or marked pending with the
[public model](/quest/m1/public-model/README.md) child that owns it. A new
model item a session reaches then fails to compile until someone makes it
public or marks it, instead of silently growing the private surface while
the other children are in flight.

## Plan

Run this before the other children so their work only shrinks the marked
set. Use the epic's inventory method: narrow every `pub(crate)` in
`model/`, then restore each item `cargo check -p moq-net --features fuzz`
(and `--tests`) names, adding a one-line comment that says which child
owns it or why it stays private.

No public API or wire change.
