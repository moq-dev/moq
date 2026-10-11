# Generated @moq/net

## Goal

moq-net is the single implementation of the MoQ protocol and model layer.
The browser runs it as TypeScript generated from the Rust source by rs2ts, a
generic Rust-subset to TypeScript translator in its own `kixelated` repo,
retiring js/net's hand-written equivalent with no regression in bundle size,
CPU, or usability. Transport glue (the WebTransport and WebSocket pumps,
timers) stays hand-written TypeScript.

This m1 line delivers the first stage: the foundation spike, the rs2ts repo,
and the generated lite codec with its go/no-go. The model, session, and IETF
stages follow in m2.

## Plan

Decided in planning (2026-09-27), with the spike data in
<https://claude.ai/artifact/8DnE9dgGN3fXvuyBwbcPJG>:

- Generated TypeScript, not WASM. Today's `moq-wasm` is 527 KB gzip against
  js/net's 84 KB, and loses on CPU to async wasm-bindgen glue (~600 ns per
  async call against 26 ns in JS). A hand-carved sans-IO lite core was 6 KB
  gzip and 1.7-9x faster than js/net, but a plain-JS port of the same
  synchronous decoder was faster still: the win is the sans-IO shape, not
  WASM. The model layer is shared too, and every call on a model handle would
  cross the WASM boundary, so generated TS is the path.
- moq-net itself becomes the sans-IO core: bytes and timestamps in, events
  and bytes out, no runtime. The async helper methods move behind an `async`
  cargo feature; rs2ts reads the crate without it and JS reimplements the
  helpers with Promises. No second crate.
- Values are plain `u64` in Rust, and varint is a wire encoding in the codec,
  not a type. The spec is not bounded to 2^53: the leading-ones form
  (moq-lite 07, moq-transport draft-17+) carries all 64 bits, and the QUIC
  form (moq-lite 01-06, drafts 14-16) refuses anything past 2^62 - 1 rather
  than truncating. Rust `u64` maps to a TypeScript `U64` (two `u32` halves),
  generically, with checked conversion to and from `number`.
- The generated TypeScript is committed and a CI lane regenerates it and
  fails on drift, so JS contributors and npm publishing never need the
  translator's toolchain. It lives inside js/net and `@moq/net` stays the
  package.
- The `@moq/net` API may change (disposable handles, `U64`) as long as it
  is no worse to use; watch, publish, hang, and the demos update in the same
  change.
- Parity: `just test interop --all`, plus moq-net's own tests translated with
  the code. They run on simulated time with no runtime (`moq-net-sim`), so the
  async-free ones translate as they stand.
- Hand-written js/net fixes keep landing until the generated path replaces
  them; it is months out.

Decided 2026-10-11 (scoping the move to its own repo):

- rs2ts is a generic tool in its own repo under `kixelated`, like quest and
  conducer: its own fixtures and CI, nothing moq-specific. moq pins it as a
  flake input and keeps the generated TS, the drift lane, and any moq-net
  source changes on its side. Why: keep the translator's toolchain out of
  moq's CI, make it reusable by other crates, iterate on it without moq PRs,
  and take a research project off moq's critical path.
- v1 scope: TypeScript output with source maps, a documented Rust subset that
  fails closed with source spans, and a runtime npm package (U64, checked
  integer ops, Option/Result and drop/refcount helpers) shared by every
  generated package so their `U64` values interoperate. Non-goals: async
  translation, full Rust, and a crates.io release.
- Spike rust-js before choosing the foundation. Planning rejected it on
  2026-09-27 for lacking Drop, generic traits, and TS output; it has since
  gained all three, plus oxc source maps on a stable rustc. Charon remains
  the default if the spike says no. See [the spike](/quest/m1/rs2ts/spike.md).
- End state stays the full lite path, staged so the line can stop at any
  stage: codec (m1), then kio and the model, then the session (m2). The
  construct inventory behind this: codec about 2.8k non-test lines and
  simple; model about 11.5k with 134 `Arc`, 60 `Weak`, 45 atomics, 21 `Drop`
  impls, and 15 `unsafe` blocks on 370 kio uses; session about 7.3k, generic
  over `transport::poll::Session` throughout.
- kio is translated too, with std's `Waker`/`Context` mapped to runtime
  types. tracing goes through a moq-net facade that generated code routes to
  the console. The model's `unsafe` storage gets a safe Rust rewrite,
  benchmarked. No separate subset lint: the regeneration lane is the check.
- moq-net refactors are planned per stage: the codec stage's now, the
  model stage's in [Generated model](/quest/m2/rs2ts-model.md) until it
  starts.
- The codec go/no-go stays before any moq-net API break, and the sans-IO lite
  session now waits on the generated model, the risky stage. The
  [async feature](/quest/m1/net-async-feature.md) gates both sessions whole,
  so it no longer waits on sans-IO and stays in m1 for Controlled time.
- Quests stay in moq until the rs2ts repo exists; translator-internal work
  then moves to that repo's `quest/` tree, and moq keeps the consumer side.
- No new `doc/` page: users see only @moq/net API changes, documented inline.
  How to regenerate goes in js/net's README beside the drift lane.

## Required

- [rust-js spike](/quest/m1/rs2ts/spike.md) - rust-js runs on moq-net's lite codec and model, and the maintainer picks the translator's foundation from the report
- [Codec refactors](/quest/m1/rs2ts/codec-refactor.md) - moq-net's lite codec extracts without the model, logs through a facade, and needs no u128
- [rs2ts repo](/quest/m1/rs2ts/repo.md) - a `kixelated` repo whose translator turns moq-net's lite codec into TypeScript with source maps, plus its runtime on npm, pinned by moq
- [Generated codec](/quest/m1/rs2ts/codec.md) - js/net's lite codec is generated, drift-checked in CI, and measured against the hand-written one: the line's go/no-go

## Related

- [Generated model](/quest/m2/rs2ts-model.md) - the next stage: kio and the model layer generated, in m2
- [Generated lite session](/quest/m2/rs2ts-session.md) - the final lite stage, in m2
- [Generated lite browser report](/quest/m2/rs2ts-browser-report.md) - the final bundle, CPU, and first-frame latency comparison after generated lite ships
- [Generated IETF](/quest/m2/rs2ts-ietf.md) - the IETF half, in m2
- [Controlled time](/quest/m1/time/README.md) - `@moq/time` is the hand-written timer glue; `moq_time::Instant` maps to it
