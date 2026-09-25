# [XS] C++ errors carry their message

## Goal

A `moq::Error` from the C++ package prints the same message Rust's `Display`
gives, so callers and the interop client log a reason rather than a variant
name.

## Plan

- The generator emits no `Display` for errors. Prefer exposing the message
  through moq-ffi once (e.g. a method on the error the generator already
  binds) over hand-written per-variant strings in the wrapper.
- Use it in the interop client and the doc samples.

Public API: additive on the C++ package, and on moq-ffi if a method is added.
Wire: none.

## Related

- [#4187](https://github.com/moq-dev/moq/pull/4187) - the package that found it
