# [L] C backend in the bindings generator

## Goal

`uniffi-bindgen-cpp` (the `kixelated` fork) gains a C backend that turns
moq-ffi's uniffi metadata into an ergonomic C header and implementation, in the
shape the [line](/quest/m1/c/README.md) decided: completion callbacks with a
cancelling task handle, a pluggable dispatcher, `moq_error *` returns with
out-params, owned record structs with free functions, and snake-case uniffi
names.

## Plan

- Build on the fork's C++ backend: it already walks the metadata, serializes
  records and errors through `RustBuffer`, and drives `rust_future_poll` on a
  dispatcher. The C backend emits the same calls behind a C surface, so the
  generated implementation may itself be C++ compiled into the package, as long
  as the public header is plain C (C99 or C11; say which).
- Cover every construct moq-ffi uses today: objects (handles), records, enums,
  the one error type, async methods, and byte buffers. moq-ffi has no callback
  interfaces; refuse them loudly rather than emit something half-working.
- The fork's test suite gains C fixtures for each construct, including
  cancelling a pending task and running callbacks through a custom dispatcher.
- Offer the backend to the LiveKit or NordSecurity upstream once its shape
  settles, or record why not.

Progress (2026-10-07): a `--lang c` backend exists on fork branch
`kixelated/c-backend` (commit not yet on GitHub: every git push to the fork
returned HTTP 500). It renders a C99 `<ns>.h` plus `<ns>_c.cpp` over the
expected-style C++ bindings, covers every construct moq-ffi uses, and
generates, compiles, and runs a read/write smoke test against moq-ffi. The
fork's `cpp-tests` gain C fixtures for each construct, cancellation, a custom
dispatcher, and refusal of callback interfaces. Remaining: land the fork PR and
tag, settle the open shape decisions in its PR, and the upstream offer.
