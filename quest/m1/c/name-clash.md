# [XS] One C name per moq-ffi item

## Goal

The generated C header gives every moq-ffi item its own name. Today the method
`MoqAudioEncoder::finish` and the record `MoqAudioEncoderFinish` both map to
`moq_audio_encoder_finish`. Waiting on [kixelated/rust2#66](https://github.com/kixelated/rust2/pull/66), which implements the rule
below; delete this quest once it merges.

## Plan

Decided with the maintainer on 2026-10-24: fix the C generator, not moq-ffi, so
no moq-ffi item is renamed. Decided 2026-10-25: the fix lands in rust2's UniFFI
fork (`uniffi/cpp/bindgen/src/bindings/c/gen_c.rs`), not in a
`kixelated/uniffi-bindgen-cpp` release, and the rule is an explicit method
namespace rather than a uniform type naming change, so types keep the short
names C users write most.

The rule:

- Types keep `<namespace>_<type>`.
- Methods and named constructors are `<type>_method_<name>`:
  `moq_audio_encoder_method_finish`, beside the record
  `moq_audio_encoder_finish`.
- The generator's fixed names stay: the primary constructor `<type>_new`, the
  release functions `<type>_free`, `<record>_default`, and `<error>_message`.
- An async call's callback typedef is its function's name plus `_callback`, so
  a method's moves with it.
- Top-level functions keep `<namespace>_<name>`, and data-enum variant structs
  keep `<enum>_<variant>`.
- Two items that still normalize to one name (`MoqFoo` and `Foo`, a function
  and a type, a type named `<Type>Free`) are refused at generation with both
  items named, and the Rust author renames one.

Under the old rule moq-ffi had exactly that one clash; under this rule it has
none, and its header compiles as C11. That check stubbed `Duration`, which
moq-ffi now uses and the C backend still refuses, another drift the generated C
line must close.

moq still builds C++ from `kixelated/uniffi-bindgen-cpp`; the rule reaches
moq's build through [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md), which
owns the pin, and the generated C line's CI build checks the header.

Public API: the generated C names of methods and named constructors gain
`_method_`. Wire: none.
