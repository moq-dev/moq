# [M] Dart binding memory leaks

## Goal

Dart bindings stop leaking native memory on every call. Two independent leaks
in the generated runtime mean `announcement.path()` in a loop grows without
bound.

## Plan

- `toRustBuffer` allocates the scratch buffer and the `ForeignBytes` that Rust
  copies from, and frees neither, so every string or struct **argument** leaks.
  Passing the `RustBuffer` itself is fine, since uniffi's callee takes
  ownership of that one.
- `rustCallWithLifter` frees the `RustCallStatus` but not the returned
  `RustBuffer`, so every returned `String`, struct, and list leaks.
  `RustBuffer.free()` appears exactly once in the whole binding, on the panic
  path.

The fix belongs upstream, not here: `dart/moq_ffi/lib/src/uniffi_runtime.dart`
is generated, and `dart/scripts/check.sh` diffs it against a fresh
`generate.sh` run, so an in-tree patch fails the staleness check by design.
Fix it in `kixelated/uniffi-dart`, release the tag, then regenerate here.

Cover it with a test that would have caught it: a loop over an accessor
returning a `String`, asserting resident memory does not grow.

This gates the first pub.dev upload, which is manual anyway (the package names
have to be claimed and trusted publishing configured), so there is a natural
place to hold it.

## Required

- [#3100](/quest/m3/3100-dart-flutter-bindings-via-uniffi.md) - the generated runtime this fixes does not exist in the tree yet
- A `kixelated/uniffi-dart` release fixing both leaks in the generated runtime
