# [L] Reach the browser through moq-ffi instead of a second hand-written wasm binding

## Goal

The browser holds the moq-ffi surface through a generated TypeScript binding
over `moq-ffi` compiled to wasm32, instead of `moq-wasm` growing a second
hand-written copy of the model (#2814 was closed for that reason). The two
`moq-wasm` gaps close by construction, because moq-ffi already binds them:
datagrams (#2822) and `track::Dynamic` so a browser publisher can serve a
cache-miss fetch (#2835). `@moq/net` stays the browser API; this is the
Rust-in-browser path `moq-wasm` experiments with today.

## Plan

The #2907 spike measured the shape: with `moq-native`, the codecs, and the
tokio runtime gated behind `cfg(not(target_arch = "wasm32"))` and uniffi's
`wasm-unstable-single-threaded` feature, 82 of 160 exported methods survive
on wasm32, the whole raw `moq-net` model among them; `uniffi-bindgen-js`
emits a `tsc --strict`-clean package where `u64` is `bigint` and a pending
call owns its own `Arc`, the two points that sank #2814; and the raw wasm is
about 8 % larger than `moq-wasm` for roughly ten times the surface.

What waits on the outside world is the generator: `uniffi-bindgen-js` is
0.2.1 with a few hundred downloads and `uniffi-bindgen-react-native` calls
itself not for production. Nothing in the spike ran in a browser.

When the gate clears:

- Gate `moq-ffi` for wasm32 as the spike did, with separate `#[cfg]`'d
  `#[uniffi::export] impl` blocks (a `#[cfg]` on a method inside one block is
  ignored by the macro). Decide `Task` semantics per target (native spawns,
  wasm awaits in place so a dropped promise cancels) and keep one
  WebTransport adapter rather than a second copy of `moq-wasm/src/transport.rs`.
- Decouple `MoqBroadcastProducer` from the hang catalog so a raw-model
  publish exists; it is the one non-`#[cfg]` change.
- Pick the generator with a browser harness in place (`test/wasm` runs the
  package in headless Chromium) and prove datagrams and a served cache-miss
  fetch end to end, including the documented drop on versions that cannot
  carry datagrams.

## Required

- [moq-mux compiles for wasm32](/quest/next/mux-wasm-target.md) - the two latent blockers, fixed now
- A `uniffi-bindgen-js` release its authors call stable, or `uniffi_bindgen` shipping a JS backend

## Closes

- [#2907](https://github.com/moq-dev/moq/issues/2907) - close this issue when the quest finishes
- [#2822](https://github.com/moq-dev/moq/issues/2822) - close this issue when the quest finishes
- [#2835](https://github.com/moq-dev/moq/issues/2835) - close this issue when the quest finishes

## Related

- [C++ through moq-ffi](/quest/next/cpp/README.md) - the same generate-not-hand-roll rule, on a generator that exists
