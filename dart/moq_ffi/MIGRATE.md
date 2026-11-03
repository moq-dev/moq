# Migrating the Dart moq\_ffi package

## Unreleased

### Media setup refusals are typed

Audio and video subscribe setup now reports `UnsupportedConfigMoqException` when the decoder refuses a codec, layout, or description. Handle this variant to try another rendition instead of inspecting an audio/video error message or `Unsupported`. Decoder faults, such as a backend that fails to open, and later peer and decode failures keep their existing errors. Regenerate bindings and rebuild the matching native library together.

### Follow the core API migration guide

These bindings are generated from moq-ffi. Apply every change in the
[moq-ffi migration guide](../../rs/moq-ffi/MIGRATE.md), using this language's generated casing.
Regenerate the bindings and rebuild the matching native library together; method metadata checksums are not a separate API migration.
The [hand-written wrapper](../moq/MIGRATE.md) has its own migration guide.

### Native futures support operation cancellation

Generated async calls return `UniffiRustFuture<T>`, which implements `Future<T>` and adds `Future<void> cancel()`.
Existing `await` and `Future<T>` assignments work unchanged; custom implementations of generated interfaces must return the new type.
Direct callers of `uniffiRustCallAsync` must pass the native cancel function before the free function.
Keep the original call to cancel it: futures returned by `then`, `timeout`, and other `Future` combinators do not propagate cancellation.
`cancel()` is idempotent and waits for native cleanup without closing the object that started the call.
The original future throws `UniffiCancelledException` if cancellation wins; a result already ready can still win the race.
Regenerate with the patched generator from `nix develop` and `just dart generate`; native symbols and UniFFI checksums are unchanged.
