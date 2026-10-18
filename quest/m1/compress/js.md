# [M] Compressed tracks in js/net

## Goal

A JS publisher sets `compress` on a track's `Info` and js/net DEFLATEs each
group on lite-07, and a JS consumer reads a compressed lite-07 track as plain
frames. On any other session a JS publisher sends plain frames without the
flag, and the refusal [Core](/quest/m1/compress/core.md) added is gone.

## Plan

See the [epic](/quest/m1/compress/README.md) for the decisions.

- Move the codec from `js/flate/src/codec.ts` into js/net, crate-private.
  `@moq/flate` is deleted in [Migrate](/quest/m1/compress/migrate.md).
- Mirror the Rust model: store compressed slices, one inflated copy per
  group shared by plain readers, the declared size enforced while inflating.
- Docs inline beside the Rust ones in `doc/concept`.

Tests: Rust and JS publish compressed tracks to each other over lite-07
through moq-relay; a JS publisher on lite-06 sends plain frames.

Public API: `Info.compress` in `@moq/net`. Wire: none beyond Core.

## Required

- [Core](/quest/m1/compress/core.md) - fixes the lite-07 wire and the Rust model this mirrors
