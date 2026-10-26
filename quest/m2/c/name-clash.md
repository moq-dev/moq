# [XS] One C name per moq-ffi item

## Goal

The generated C header gives every moq-ffi item its own name. Today the method
`MoqAudioEncoder::finish` and the record `MoqAudioEncoderFinish` both map to
`moq_audio_encoder_finish`. Waiting on
[kixelated/rust2#66](https://github.com/kixelated/rust2/pull/66), which
implements the rule below; delete this quest once it merges.

## Plan

Decided with the maintainer on 2026-10-24: fix the C generator, not moq-ffi, so
no moq-ffi item is renamed. Decided 2026-10-25: the fix lands in rust2's UniFFI
fork (`uniffi/cpp/bindgen/src/bindings/c/gen_c.rs`), not in a
`kixelated/uniffi-bindgen-cpp` release.

The naming rule (maintainer, 2026-10-25):

- ✅ GLib/GStreamer style: CamelCase types, snake_case functions
- Explicit method namespace (`moq_audio_encoder_method_finish`), chosen first
  the same day and then superseded by the GLib rule
- Uniform type naming change while keeping short function names
- Path names plus a `[rename]` table

Why: function calls read naturally (`moq_audio_encoder_finish(enc)`), C users
of GStreamer and OBS already know the convention, and a type can never share an
identifier with a function, so no rename table is needed. A rename table would
make names unpredictable: adding an item could silently rename an existing one.

The rule:

- Types are CamelCase with the namespace prefix: `MoqAudioEncoder`,
  `MoqAudioEncoderFinish`, `MoqError`, `MoqTask`, compounds such as
  `MoqStringList`, enum tags `<Enum>Tag`, variant structs `<Enum><Variant>`,
  and async callback typedefs `<Function>Callback`
  (`MoqAudioEncoderFinishCallback`). A type's name is its prefixed snake-case
  name re-cased, so it is predictable from its functions' prefix.
- Functions stay snake_case with no infix: `moq_audio_encoder_finish`,
  `moq_audio_encoder_new`, `moq_audio_encoder_free`, `moq_error_message`. Enum
  constants stay `MOQ_<ENUM>_<VARIANT>`.
- Any clash that remains (two functions with one name, such as a method `free`
  and the release function, or `MoqFoo` and `Foo`) is refused at generation
  with both items named, and the Rust author renames one.

rust2's own exporter (`ffi/`), which replaces the fork, adopts the same rule
instead of path names plus a `[rename]` table, so moq's C names do not change
again when it lands.

Under the old rule moq-ffi had exactly the one clash; under this rule it has
none, and its header compiles as C11. That check stubbed `Duration`, which
moq-ffi now uses and the C backend still refuses, another drift the generated C
line must close.

moq still builds C++ from `kixelated/uniffi-bindgen-cpp`; the rule reaches
moq's build through [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md), which
owns the pin, and the generated C line's CI build checks the header. The
generated moq-c package
([moq-dev/moq#6052](https://github.com/moq-dev/moq/pull/6052)) adopts these
names then.

Public API: generated C type names become CamelCase; function names are
unchanged. Wire: none.
