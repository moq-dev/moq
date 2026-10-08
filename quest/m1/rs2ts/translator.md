# [L] rs2ts

## Goal

`rs/rs2ts` translates moq-net's lite codec into readable TypeScript inside
js/net. The output is committed, a CI lane regenerates it and fails on
drift, and the generated codec passes `just test interop --all` in place of
the hand-written one. The PR carries the line's go/no-go: the generated
codec's bundle size and per-frame CPU against js/net's hand-written codec. A
no-go stops the [line](/quest/m1/rs2ts/README.md) before the sans-IO work.

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
  `model/resume.rs`'s `end()` returns `Option<Option<Result<()>>>` to tell a
  front that may still replace its copy (`None`) from one that concluded
  (`Some(None)`), and its `until: Option<Option<u64>>` does the same for a
  replaced route. Recommendation: the subset lint rejects nested `Option`, and the source
  names those states with an enum; a tagged TypeScript form for the inner
  `Option` is the alternative. Either way nested states never merge silently.
- `Drop` becomes an explicit `drop()` at each MIR drop point, exposed as
  `[Symbol.dispose]`; `Arc`/`Rc` of a type with drop glue become an explicit
  refcount. JS is single-threaded, so `Mutex` and atomics become plain
  access.
- Rust `u64` maps to js/net's `U64` (`js/net/src/util/u64.ts`, two `u32`
  halves), generically, read and written as a varint by `Cursor.varint()` and
  `Writer.varint()`. Integers up to 32 bits and `usize` map to `number` with
  checked arithmetic that throws on overflow; never wrap silently. A `u64` or
  `i64` never maps to a lossy `number`: the model accepts `u64::MAX` (e.g.
  `model/subscription.rs`). Varint is a wire encoding in the codec, not a
  type, so nothing maps by that name.

Guidance:

- Keep rs2ts a generic translator for a Rust subset, not a moq-net tool. It
  maps by Rust type and construct (e.g. `u64` to one 64-bit TypeScript type),
  never by moq-net names; anything project-specific lives in moq-net's source
  or a small config, so another crate could use rs2ts unchanged.
- `varint::zigzag` and `unzigzag` (lite per-frame timestamps) still use
  64-bit bit math, which the subset forbids. Rewrite them on two `u32` halves,
  or give the 64-bit TypeScript type the operations they need.
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
