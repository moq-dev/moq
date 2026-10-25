# [S] One C name per moq-ffi item

## Goal

The generated C header gives every moq-ffi item its own name. Today the method
`MoqAudioEncoder::finish` and the record `MoqAudioEncoderFinish` both map to
`moq_audio_encoder_finish`.

## Plan

Decided with the maintainer on 2026-10-24: fix the C generator, not moq-ffi. The
generator derives collision-free names, for example by qualifying a method with
its type, so no moq-ffi item is renamed.

The generator lives in [kixelated/rust2](https://github.com/kixelated/rust2)'s
UniFFI fork (`uniffi/`), so the work lands there. This quest stands in for it
until moq builds from a fork that ships the fix.

The implementation is in
`uniffi/cpp/bindgen/src/bindings/c/gen_c.rs` in rust2. `Gen::render` reserves
all type names first, `render_object` then names each method
`<type>_<method>`, and `claim` refuses a duplicate rather than generating an
invalid header. The same ambiguity reaches constructors, generated `_free`
functions, and async `_callback` typedefs; changing only `finish` leaves the
naming rule ambiguous.

Open maintainer decision: choose the generated C naming rule before changing
symbols. Recommendation: put methods under an explicit method namespace, for
example `moq_audio_encoder_method_finish`, while preserving record names.
Alternative: change generated type names uniformly and preserve the short
function names. Check the chosen rule against constructors, release functions,
async callbacks, and normalized-name collisions, with a compiled C fixture in
rust2. No moq-ffi item should be renamed.

After the generator fix lands, update moq's generator pin to a revision that
contains it and prove that the moq-ffi header generates and compiles. Today
`flake.nix` still consumes the separate `kixelated/uniffi-bindgen-cpp` fork;
the fix must reach the generator moq actually builds.

Public API: the generated C symbols changed by the chosen naming rule. Wire: none.
