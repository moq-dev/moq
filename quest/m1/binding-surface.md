# [M] Audio decode delay in every binding

## Goal

moq-ffi and every wrapper (Python, Go, Swift, Kotlin, Dart) expose
`decode::Options::delay` and `decode::Consumer::delay()` so applications can
configure and observe audio playout delay.

## Plan

- One PR, each wrapper touched once, in its own idiom: durations as the
  language's duration type where the wrapper already uses one, handles over
  flat methods where a surface has more than one call.
- moq-ffi only, not the hand-written moq-c: the [generated C](/quest/m1/c/README.md) and
  C++ bindings inherit it from moq-ffi.
- Update `doc/lib/{py,swift,kt,go,dart}` in the same PR.
- Test configuration and observed delay in every wrapper that has tests.

Decided in the 2026-10-06 audit: the native decode delay API is on `main`
(#4162, `rs/moq-audio/src/decode/consumer.rs`), so this no longer waits on
the m0 jitter-target line, whose remaining Watch proof bindings do not use.
It waits on Codecs instead, so each wrapper adds delay to the reshaped audio
decoder rather than to `BroadcastConsumer.decode_audio`, which Codecs removes.

## Required

- [Codecs](/quest/m1/ffi-shape/codec.md) - the reshaped audio decoder each wrapper adds delay to

## Related

- [FFI shape](/quest/m1/ffi-shape/README.md) - reshapes the binding namespaces
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - landed the native decode delay API this exposes
