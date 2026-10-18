# [M] One encrypted consumer shape for every player

## Goal

Encrypted playback has one consumer shape, the mirror of the protected
producer, that the browser, the CLI, and the FFI bindings share, so each
player does not reinvent decryption and physical-name mapping.

## Plan

#5822's CLI plan names the consumer as "the consuming mirror of that
producer in `moq-mux`", but nothing gives it a shape the browser and FFI
can mirror. Mock up the Rust shape first, then mirror the names in JS and
the bindings. Kept in m2 (decided 2026-10-17).

The moq-ffi consumer follows the `rs/moq-ffi` row of the root `AGENTS.md`
cross-package table: wrappers, C and C++, OBS where it plays, and the
language docs land with it, or the FFI part waits for a quest that does.

Public API: a new e2ee consumer in `moq-mux`, `@moq/e2ee`, and moq-ffi with
its wrappers. Wire: none.

## Required

- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - the producer this mirrors
- [Dynamic names](/quest/m2/e2ee/dynamic-names.md) - the approved naming design can change the physical-name mapping this fixes
