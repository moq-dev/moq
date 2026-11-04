# Migrating the Kotlin moq-ffi package

## Unreleased

### Viewer targets are bounded ([#7090](https://github.com/moq-dev/moq/pull/7090))

Viewer ceilings and PROBE target overrides are refused with the new `MoqException.OutOfRange` once they would sum above 2^53 - 1 bps, the most a PROBE target carries; nothing changes on a refusal. `MoqViewers.watch` and `setTarget` and `MoqViewer.update` throw it. Handle the new variant wherever you match the error exhaustively. Regenerate bindings and rebuild the matching native library together.

### Media setup refusals are typed

Audio and video subscribe setup now reports `MoqException.UnsupportedConfig` when the decoder refuses a codec, layout, or description. Handle this variant to try another rendition instead of inspecting an audio/video error message or `Unsupported`. Decoder faults, such as a backend that fails to open, and later peer and decode failures keep their existing errors. Regenerate bindings and rebuild the matching native library together.

### Follow the core API migration guide

These bindings are generated from moq-ffi. Apply every change in the
[moq-ffi migration guide](../../rs/moq-ffi/MIGRATE.md), using this language's generated casing.
Regenerate the bindings and rebuild the matching native library together; method metadata checksums are not a separate API migration.
The [hand-written wrapper](../moq/MIGRATE.md) has its own migration guide.
