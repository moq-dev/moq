# [L] rs2ts repo

## Goal

A new `kixelated` repo holds rs2ts, a generic translator from a documented
Rust subset to readable TypeScript with source maps, and its runtime package
published to npm. It is tested on its own fixtures, and it translates
moq-net's lite codec. moq pins a tagged rev as a flake input. The draft
prototype [#5049](https://github.com/moq-dev/moq/pull/5049) is closed, with
its fixtures ported.

## Plan

Written for the Charon default. [The spike](/quest/m1/rs2ts/spike.md)
rewrites this quest if the maintainer picks another foundation.

Decided 2026-10-11:

- Generic: rs2ts maps by Rust type and construct, never by moq-net names.
  Anything project-specific lives in the source crate or a small config, such
  as extern bindings from a Rust item to hand-written TS (`moq_time::Instant`
  to `@moq/time`, the tracing facade's hook).
- v1 ships TypeScript with source maps, and fails closed: an unsupported
  construct is a generation error with a source span. That error is the
  subset check, so no separate clippy or dylint lint. Document the subset in
  the README. Async translation and full Rust are non-goals.
- Runtime on npm (the name is open; the maintainer suggested an `@r2t/std`
  shape): `U64` (moved from `js/net/src/util/u64.ts`), checked integer ops,
  Option/Result and drop/refcount helpers. Every generated package shares it,
  so their `U64` values interoperate.
- The translator is distributed as a flake (or `cargo install --git`), not on
  crates.io: crates.io refuses git dependencies, and `charon_lib` is
  unpublished.
- Close #5049 (comment first, linking the repo) once its comparison cases and
  ownership probes are ported. Its Python emitter is not carried over.
- Later translator work (kio and the model subset) moves to this repo's own
  `quest/` tree once it exists, starting with
  [model support](/quest/m2/rs2ts-model-support.md).

Open, for this quest: the repo and crate name. `rs2ts` is free on crates.io,
but unscoped `rs2ts` on npm is taken by a TS interface generator, and the
name usually means type generation, not code translation.

Charon (from the 2026-10-11 research):

- Pin one rev in two places: a flake input for the binary and its nightly,
  and `charon_lib = { package = "charon", git, tag = "nightly-YYYY.MM.DD",
  default-features = false }`. A CI check fails if they differ, and a bump
  script moves both, following Eurydice. The LLBC format breaks about five
  times a week, and Charon's CI tests Aeneas and Eurydice but not us, so
  bumps are deliberate.
- `--precise-drops` gives the exact drop points that close-on-last-drop
  handles need; `--start-from` extracts a subset. Charon stalled for 40+
  minutes on the whole of moq-net, so extract only what is generated.
- Tests: snapshot the generated TS from sample crates (as Eurydice and Aeneas
  do), plus differential runs against native Rust (as #5049 and rust-js do).

Mapping decided in planning:

- Structs become classes, enums discriminated unions, traits interfaces.
  `Option<T>` is `T | undefined`, `&[u8]` a `Uint8Array` view with no copy.
  A nested `Option` would collapse, so reject it; the source names those
  states with an enum (`model/resume.rs` has two, for the model stage).
- `Drop` becomes an explicit `drop()` at each MIR drop point, exposed as
  `[Symbol.dispose]`; `Arc`/`Rc` of a type with drop glue become an explicit
  refcount. JS is single-threaded, so `Mutex` and atomics become plain
  access.
- Rust `u64` maps to the runtime's `U64`, including its bit operations.
  Integers up to 32 bits and `usize` map to `number` with checked arithmetic
  that throws on overflow; never wrap silently. A `u64` or `i64` never maps to
  a lossy `number`: the model accepts `u64::MAX`.
- Maps and sets keyed by `U64` or structs need runtime collections keyed by
  derived `Eq`/`Hash`/`Ord`, since a JS `Map` compares objects by identity.
  Use a plain `Map` only for number and string keys. `BTreeMap` needs an
  ordered map with `range`.

Guidance:

- Borrow rust-js's MIT oxc printer for formatting and source maps.
- Readability pass: inline single-use temporaries and keep source branch
  order, so a reviewer can read a generated diff.
- The moq PR for this quest adds the flake input and puts rs2ts in the dev
  shell, nothing else.

Public API: a new tool and npm package outside moq. Wire: none.

## Required

- [rust-js spike](/quest/m1/rs2ts/spike.md) - decides the foundation
