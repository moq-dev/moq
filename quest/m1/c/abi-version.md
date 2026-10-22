# [S] A C caller cannot be laid out against a newer library

## Goal

A program built against an older `moq.h` cannot have its config struct overflowed by a newer library, and an early failure is documented as having no handle and no callback:

- Each `#[repr(C)]` config struct carries its own size, set by the caller, and the library refuses a size it does not know.
- `moq_client_defaults` fills a caller-owned struct up to the caller's size, rather than returning one by value.
- `moq_session_connect` and every other `on_status` registration say that a negative return means no handle and no callback, so the caller owns `user_data`.
- A spawned callback's cleanup cannot be lost to a poisoned mutex.

Today the config structs have no size field, `moq_client_defaults()` returns its struct by value, and `moq_session_connect` says to call close even on error. The bandwidth change wait calls `callback.call` outside `ffi::run`, so a panic there drops the terminal callback silently.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Add a leading `size` field to `moq_client_config` and the other config structs, and a `moq_abi_version()` for diagnostics. Zero is refused, so a caller that forgets the field fails loudly rather than reading uninitialized fields.
- Change `moq_client_defaults()` to take a pointer to the caller's struct and return a status. A by-value return of a struct that grows changes the return slot, and possibly the calling convention, before either side can read `size`, so the field alone does not protect it.
- These are themselves ABI breaks for an already-compiled caller, which a migration note cannot reach: an old binary would call the new `moq_client_defaults` with the old convention and pass structs without the leading field. Give the shared library a SONAME (and macOS install name) that changes with each breaking release, so an old binary fails to load instead of miscalling.
- Fix the "adding a field does not disturb existing callers" comment: it is true only when both sides agree on the size.
- Document the immediate-failure contract on every registration that returns a handle.
- Move the spawned cleanup inside `ffi::run`, or match `state.rs`'s `PoisonError::into_inner`.

Tests: a zero size is refused; a short size is refused; defaults written for an older size touch no byte past it; the documented failure paths fire no callback.

Public API: a breaking ABI (the leading field and the `moq_client_defaults` signature) with a `MIGRATE.md` entry, one new function, and a versioned SONAME; `cpp/moq` and the Dart bindings follow. Wire: none.

## Related

- [Generated C bindings](/quest/m1/c/README.md) - the ABI this hardens before more consumers adopt it
