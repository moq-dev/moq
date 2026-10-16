# [M] Bindings carry catalog reference epochs

## Goal

The UniFFI bindings and the C ABI expose a catalog reference's optional
`epoch` and honor it when resolving, so a native app gets the same guarantee
as Rust and `@moq/watch`: a broadcast proven to serve another instance fails
the rendition instead of playing it.

## Plan

Today `MoqVideo` and `MoqAudio` drop the field, and `MoqBroadcastConsumer::resolve`,
`decode_video`, `decode_audio` and libmoq's rendition resolution pass `None`
to `moq_mux::Source::resolve`, so they read like a reader that predates the
field: the first instance resolved, without its guarantee.

- Add `epoch` beside `broadcast` on the binding records, and pass it from
  `decode_video` and `decode_audio`.
- Decide the `resolve` shape with the maintainer: a second optional argument,
  or one reference record carrying `broadcast` and `epoch`.
- Carry it through the hand-written wrappers and docs in the Cross-Package
  Sync table (`rs/moq-c`, `py`, `swift`, `kt`, `go/wrapper`, `dart`, `cpp`,
  `doc/lib`).

Test a matching, mismatched and epochless reference through `moq-ffi` and
libmoq, and run `just test interop --all`.

## Related

- [Rust readers follow references](/quest/m0/broadcast-epoch/reference-announcements.md) - the following behavior the bindings inherit from `moq_mux`
