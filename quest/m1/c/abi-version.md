# [S] A C caller cannot be laid out against a newer library

## Goal

A program built against an older `moq.h` cannot have its config struct overflowed by a newer library, and an early failure is documented as having no handle and no callback:

- Each `#[repr(C)]` config struct carries its own size, set by the caller, and the library refuses a size it does not know.
- `moq_client_defaults()` cannot write a larger struct into an older caller's slot.
- `moq_session_connect` and every other `on_status` registration say that a negative return means no handle and no callback, so the caller owns `user_data`.
- A spawned callback's cleanup cannot be lost to a poisoned mutex.

Today the config structs have no size field, `moq_client_defaults()` returns by value, and `moq_session_connect` says to call close even on error. The bandwidth change wait calls `callback.call` outside `ffi::run`, so a panic there drops the terminal callback silently.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add a leading `size` field to `moq_client_config` and the other config structs, and a `moq_abi_version()` for diagnostics. Zero is refused, so a caller that forgets the field fails loudly rather than reading uninitialized fields.
- Fix the "adding a field does not disturb existing callers" comment: it is true only when both sides agree on the size.
- Document the immediate-failure contract on every registration that returns a handle.
- Move the spawned cleanup inside `ffi::run`, or match `state.rs`'s `PoisonError::into_inner`.

Tests: a zero size is refused; a short size is refused; the documented failure paths fire no callback.

Public API: additive fields and one function. Wire: none.

## Related

- [Generated C bindings](/quest/m1/c/README.md) - the ABI this hardens before more consumers adopt it
