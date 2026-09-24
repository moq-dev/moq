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

That upstream fix is merged as
[kixelated/uniffi-dart#1](https://github.com/kixelated/uniffi-dart/pull/1), on
the `uniffi-0.32` branch rather than `main`, which carries a different
(`0.31.2`) line. What remains here: tag it, repin `flake.nix` and
`nix/uniffi-dart-Cargo.lock`, and run `just dart generate`.

Freeing the returned buffer alone would have been a use-after-free, which is
probably why nothing freed it. `BytesCodeType.read` returned a `Uint8List.view`
over the buffer, so a lifted byte array aliased Rust memory and escaped into
the caller. The byte read has to copy first; only then is the free sound.

Expect red CI on that fork: `futures_test.dart: sleep` and the payjoin
downstream job both fail on an unmodified `uniffi-0.32`, confirmed by a
baseline run. Neither is caused by the fix, and `bytes_types`, the fixture
that covers it, passes.

Cover it with a test that would have caught it: a loop over an accessor
returning a `String`, asserting resident memory does not grow.

This gates the first pub.dev upload, which is manual anyway (the package names
have to be claimed and trusted publishing configured), so there is a natural
place to hold it.

## Required

- A `kixelated/uniffi-dart` tag on `uniffi-0.32` containing the merged fix
