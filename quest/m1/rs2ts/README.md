# Generated @moq/net

## Goal

moq-net is the single implementation of the MoQ protocol and model layer.
The browser runs it as TypeScript generated from the Rust source, retiring
js/net's hand-written equivalent with no regression in bundle size, CPU, or
usability. Lite is this line; IETF follows in m2. Transport glue (the WebTransport and
WebSocket pumps, timers) stays hand-written TypeScript.

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
- The translator is `rs/rs2ts`, built on Charon (Rust MIR restructured into
  LLBC). Charon's `--precise-drops` gives exact drop points, which the
  close-on-last-drop handles depend on, and `--start-from` extracts a subset.
  rust-js was evaluated and rejected as a base: no Drop, no generic traits,
  JS only, all-or-nothing extraction, 32-bit `usize`. Its MIT oxc printer is
  worth borrowing for formatting and source maps.
- moq-net itself becomes the sans-IO core: bytes and timestamps in, events
  and bytes out, no runtime. The async helper methods move behind an `async`
  cargo feature; rs2ts reads the crate without it and JS reimplements the
  helpers with Promises. No second crate.
- Values are plain `u64` in Rust, and varint is a wire encoding in the codec,
  not a type. The spec is not bounded to 2^53: the leading-ones form
  (moq-transport draft-17+) carries all 64 bits, and the QUIC form (moq-lite,
  drafts 14-16) refuses anything past 2^62 - 1 rather than truncating. Rust
  `u64` maps to a TypeScript `U64` (two `u32` halves), generically, with
  checked conversion to and from `number`.
- The generated TypeScript is committed and a CI lane regenerates it and
  fails on drift, so JS contributors and npm publishing never need the
  nightly toolchain Charon pins. It lives inside js/net and `@moq/net` stays
  the package.
- The `@moq/net` API may change (disposable handles, `U64`) as long as it
  is no worse to use; watch, publish, hang, and the demos update in the same
  change.
- Parity: `just test interop --all`, plus moq-net's own tests translated with
  the code. They run on simulated time with no runtime (`moq-net-sim`), so the
  async-free ones translate as they stand.
- The Rust refactors break moq-net's published API, and the translator and
  generated code build on them.
- Hand-written js/net fixes keep landing until the generated path replaces
  them; it is months out.

Decided in the 2026-09-30 audit: the lite half stays in m1 with an explicit
go/no-go after the no-downgrade report below; a no-go stops the line before
anything else is generated. The IETF half (the sans-IO IETF session,
generated IETF, and IETF parameters) moved to m2 and waits on that go.

This README's own work is the no-downgrade report once generated lite ships:
bundle size, per-frame CPU, and first-frame latency against the hand-written
js/net it replaces, measured with the [browser benchmarks](/quest/m1/browser-benchmarks.md).

## Required

- [Browser benchmarks](/quest/m1/browser-benchmarks.md) - the harness the no-downgrade report uses
- [rs2ts](/quest/m1/rs2ts/translator.md) - a Charon-based translator emits readable TypeScript for moq-net's lite codec, committed and checked for drift in CI
- [Sans-IO moq-net](/quest/m1/rs2ts/sans-io/README.md) - moq-net builds and runs without a runtime; async helpers sit behind an `async` feature
- [Generated lite](/quest/m1/rs2ts/lite.md) - @moq/net's lite session and model layer are generated from moq-net
- [Remove moq-wasm](/quest/m1/rs2ts/remove-wasm.md) - the WASM experiment is deleted once generated lite ships

## Closes

- [#2907](https://github.com/moq-dev/moq/issues/2907) - close this issue when the quest finishes
- [#2822](https://github.com/moq-dev/moq/issues/2822) - close this issue when the quest finishes
- [#2835](https://github.com/moq-dev/moq/issues/2835) - close this issue when the quest finishes

## Related

- [#2850](/quest/m1/2850-js-net-give-reader-a-synchronous-decode-so-the-publisher.md) - caps hand-written js/net's subscription controls; generated lite replaces the rest
- [Generated IETF](/quest/m2/rs2ts-ietf.md) - the IETF half, deferred to m2 until the lite go/no-go
