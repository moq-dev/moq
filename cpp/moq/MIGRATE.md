# Migrating the C++ MoQ package

## Unreleased

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Publishing, serving, or requesting a broadcast at a path with a leading, trailing, or doubled `/` fails instead of collapsing it. Pass the canonical path, such as `room/cam`.

### First release

There is no published C++ package to migrate from. Start with the current headers and examples, and build the wrapper, generated bindings, and native library together.
The underlying generated API follows the [moq-ffi migration guide](../../rs/moq-ffi/MIGRATE.md).
