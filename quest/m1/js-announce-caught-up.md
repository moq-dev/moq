# [S] @moq/net announce consumers know when they have caught up

## Goal

`@moq/net`'s announce consumer yields the same `Live` marker the Rust
consumer gets from [Caught up](/quest/m1/cli-inspect/caught-up.md), with
the same ordering and per-source semantics, so a browser app can render "no
broadcasts" instead of a spinner that never resolves.

## Plan

Mirror the Rust shape, name, and marker-less fallback: one flat event,
`Announced`, `Updated`, `Retracted` (each carrying the announce), or `Live`,
replacing the update's `kind` field. Test the same cases
in JS. Public API: the announce consumer's yield type changes, so it lands
with the Rust break. Wire: none.

## Required

- [Caught up](/quest/m1/cli-inspect/caught-up.md) - settles the API shape this mirrors
