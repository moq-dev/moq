# [XS] Waiting: ubrn generates UniFFI 0.32 TypeScript

## Goal

uniffi-bindgen-react-native (ubrn) ships a release whose `wasm2` flavour reads UniFFI
0.32 metadata, so moq-ffi's browser TypeScript generates from the published crate
without `test/wasm/uniffi/pin.patch`. Check: whether
[ubrn#468](https://github.com/jhugman/uniffi-bindgen-react-native/pull/468) has merged and
a release after 0.31.0-6 lists it. Delete this quest once the pin is gone.

## Plan

Decided with the maintainer on 2026-10-17 (#5838): the browser binds moq-ffi through
UniFFI over ubrn's `wasm2` player. ubrn pins UniFFI 0.31; moq-ffi is on 0.32, and the
harness pins a scratch checkout to 0.31 for measurement only.

If ubrn has not moved by the time adoption is otherwise ready, carry ubrn's TypeScript
generator in [the UniFFI fork](/quest/m2/uniffi-fork.md) instead, and delete this quest.

When raising it upstream, also report two costs the spike measured. Posting outside
`moq-dev` and `kixelated` needs the maintainer's approval.

- `@ubjs/core` arms and clears a `setTimeout` keep-alive on every async call, which only
  Node.js needs.
- The generated `*Like` return types omit `uniffiDestroy`, so freeing a returned handle
  early needs a cast to the class.

## Related

- [Adopt WASM in @moq/net](/quest/m2/rs2ts/wasm-adopt.md) - ships the generated TypeScript this unblocks
