# [L] rs2ts

## Goal

`rs/rs2ts` translates moq-net's lite codec into readable TypeScript inside
js/net. The output is committed, a CI lane regenerates it and fails on
drift, and the generated codec passes `just test interop --all` in place of
the hand-written one.

## Plan

A prototype exists in the planning spike: about 1,800 lines on `charon_lib`
plus a 200-line runtime shim. It turned a sample crate into TypeScript that
passed behavioral tests, including close-on-last-drop, and ran on moq-net's
`coding` and lite message modules via `--start-from` (about 7,000 lines of
output, 500 untranslated calls, mostly std, tracing, and atomics shims).

Mapping decided in planning:

- Structs become classes, enums discriminated unions, traits interfaces.
  `Option<T>` is `T | undefined`, `&[u8]` a `Uint8Array` view with no copy.
  That collapses a nested `Option`, whose states the source relies on:
  `model/track.rs::first_start` returns `Option<Option<Timestamp>>` to tell
  "no successor" from an unstamped one, and `reach` behaves differently for
  each. Recommendation: the subset lint rejects nested `Option`, and the source
  names those states with an enum; a tagged TypeScript form for the inner
  `Option` is the alternative. Either way nested states never merge silently.
- `Drop` becomes an explicit `drop()` at each MIR drop point, exposed as
  `[Symbol.dispose]`; `Arc`/`Rc` of a type with drop glue become an explicit
  refcount. JS is single-threaded, so `Mutex` and atomics become plain
  access.
- Rust `u64` (and `VarInt` while it lasts) maps to js/net's `U64`
  (`js/net/src/util/u64.ts`), read and written as a varint by
  `Cursor.varint()` and `Writer.varint()`.
  Integers up to 32 bits and `usize` map to `number` with checked arithmetic
  that throws on overflow; never wrap silently. A `u64` or `i64` never maps
  to a lossy `number`: the model accepts `u64::MAX` (e.g.
  `model/subscription.rs`), so each one either becomes `U64` or an
  `Option` in the source, or maps to a full-width 64-bit TypeScript type.

Guidance:

- Pin Charon and its nightly in the nix shell for the regeneration lane only.
- Borrow rust-js's MIT oxc printer for formatting and source maps.
- Readability pass: inline single-use temporaries and keep source branch
  order, so a reviewer can read a generated diff.
- Add a lint on moq-net (clippy or dylint) for the accepted subset: no
  `unsafe`, no `async` outside the `async` feature, no nested `Option`, no `u64` bit operations,
  no trait impls on foreign or primitive types, no `&mut` out-params to
  scalar or `Option` locals. Translator gaps become compile errors, not
  runtime `todo()`s. Document the subset in `rs/rs2ts/README.md`.
- Charon stalled for 40+ minutes on the whole crate; extract only the modules
  being generated.

Public API: none (internal tool). Lands with the codec it
translates. Wire: none.

## Required

- [VarInt codec](/quest/m1/rs2ts/varint-codec.md) - the codec shape the translator targets
