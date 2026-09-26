# [XS] Kotlin end

## Goal

Kotlin can force-end a broadcast after `finish` is removed. Its
`MoqBroadcastProducer` exposes the moq-ffi `close()` as `end()`, because a
generated `close()` would collide with `AutoCloseable.close()`.

## Plan

- Rename the method for Kotlin only in `rs/moq-ffi/uniffi.toml`; every other
  binding keeps `close()`. Kotlin's `close()` still only releases the handle.
- Test that `end()` ends the broadcast while a `dynamic()` handle is still
  held. Document it in `doc/lib/kt`.

## Required

- [Binding close](/quest/m1/broadcast-close/bindings.md) - moq-ffi gains the `close()` this renames
