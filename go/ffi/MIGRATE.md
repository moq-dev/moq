# Migrating the Go moq-ffi module

## Unreleased

### Follow the core API migration guide

These bindings are generated from moq-ffi. Apply every change in the
[moq-ffi migration guide](../../rs/moq-ffi/MIGRATE.md), using this language's generated casing.
Regenerate the bindings and rebuild the matching native library together; method metadata checksums are not a separate API migration.
The [hand-written wrapper](../wrapper/MIGRATE.md) has its own migration guide.
