# [M] Audio decode delay in every binding

## Goal

moq-ffi, libmoq, and every wrapper (Python, Go, Swift, Kotlin, Dart) expose
`decode::Options::delay` and `decode::Consumer::delay()` so applications can
configure and observe audio playout delay.

## Plan

- One PR, each wrapper touched once, in its own idiom: durations as the
  language's duration type where the wrapper already uses one, handles over
  flat methods where a surface has more than one call.
- Add the libmoq implementation and tests in `rs/libmoq`, then regenerate
  `moq.h`. Keep the additions compatible with the published C ABI.
- Update `doc/lib/{py,swift,kt,go,dart,c}` in the same PR.
- Test configuration and observed delay in every wrapper that has tests.

## Required

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - lands the native decode delay API on `main`; #3967 merged into the questline only

## Related

- [FFI shape](/quest/m1/ffi-shape/README.md) - reshapes the binding namespaces
