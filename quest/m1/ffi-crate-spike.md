# [L] Prove generated binding namespaces

## Goal

Decide whether every supported binding generator can expose per-layer
namespaces from one native library before this release freezes the API.

## Plan

Spike, splitting `media` from `net` alone:

1. Make the dependency one-way, `media` on `net`. Today `net`'s consumers
   return `MoqFrame` and `MoqDatagram` from `media.rs`, which Cargo would
   reject as a cycle; those raw records likely belong in `net`.
2. Generate bindings from the built library, in this order: uniffi-bindgen-go,
   uniffi-bindgen-dart, uniffi-bindgen-cpp (both C++ and generated C), then Kotlin, Swift, and Python.
   For each, check:
   - It emits one module per crate.
   - `net` types used from `media` resolve as external types, covering each
     category the real split crosses: an object (`GroupConsumer`), the
     thrown `MoqError`, and a record or enum.

   The Go, Dart, and C++ generators are kixelated forks pinned in
   `flake.nix`, so patch a gap in the fork before calling it a no-go.
   For generated C, compile and link an installed consumer using cross-crate
   objects, errors and records from the same native library.
3. Check that each platform still ships one native library: the `go/ffi`
   module layout, the Swift XCFramework and its module maps, the Kotlin and
   Android artifacts, the `moq_ffi` Dart package, the Python wheel, and the
   C++ CMake package.
4. Record the verdict for each generator here, along with the library size
   before and after the split.

A go enables [the migration](/quest/m1/ffi-crate-split.md). A measured
no-go records the unsupported generator and retains wrapper namespaces,
removing the migration prerequisite from the tree. This quest ships proof
and a decision, not the full wrapper migration.
