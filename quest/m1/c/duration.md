# [S] Durations in generated C

## Goal

The C backend of the `kixelated/uniffi-bindgen-cpp` fork renders UniFFI's
builtin `Duration` instead of bailing with "the C backend does not support
`Duration` yet", so moq-ffi can export typed durations without losing the
generated C. A tagged fork release carries it and moq pins that tag.

## Plan

Found 2026-10-23 while checking [binding durations](/quest/m1/ffi-durations.md):
Go, Dart, C++, Python, Swift, and Kotlin already map `Duration`; only
`bindings/c/gen_c.rs` refuses it, alongside `Timestamp`, `Set`, and `Custom`.

- Pick the C spelling to mirror the C++ package, which uses
  `std::chrono::duration<int64_t, std::nano>`. A single nanosecond count
  keeps C callers free of a struct for the common case; check that it covers
  every value moq-ffi exposes and refuses the rest loudly.
- Test it with a fixture in the fork, both directions and inside records and
  optionals, the way the fork tests its other C types.
- Tag the fork with the next `-kixelated.N` and move every place that names
  the pinned tag together (the flake's comment lists them).

Public API: none until moq-ffi exports a `Duration`. Wire: none.
