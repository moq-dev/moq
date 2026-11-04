# Migrating the Dart moq\_ffi package

## Unreleased

### Separate datagram tracks

Generated bindings now expose separate `MoqDatagramProducer`, `MoqDatagramConsumer`, `MoqDatagramInfo`, `MoqDatagramRequest`, and `MoqDatagramRequests` types. Create or subscribe through the broadcast's datagram methods, write with `append` / `insert`, and read with `recv`; stream-track datagram methods are removed. Demand is aggregate subscriber priority, and the broadcast request handler has a separate `datagrams` queue. Regenerate and rebuild native bindings together with your wrapper.

### Viewer targets are bounded ([#7090](https://github.com/moq-dev/moq/pull/7090))

Viewer ceilings and PROBE target overrides are refused with the new `OutOfRangeMoqException` once they would sum above 2^53 - 1 bps, the most a PROBE target carries; nothing changes on a refusal. `MoqViewers.watch` and `setTarget` and `MoqViewer.update` throw it. Handle the new variant wherever you match the error exhaustively. Regenerate bindings and rebuild the matching native library together.

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
