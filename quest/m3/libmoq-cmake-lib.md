# [XS] libmoq CMake finds the library cargo built

## Goal

`rs/libmoq/CMakeLists.txt` links the static library cargo actually produced,
whatever `CARGO_TARGET_DIR`, `--target` triple, or profile the build uses.
Today `BUILD_RUST_LIB` assumes `target/<debug|release>/libmoq.a`, so any other
layout links a stale library or fails to find one.

## Plan

Parked in m3 behind [Generated C bindings](/quest/m1/c/README.md): the hand-written libmoq is being replaced by C generated from moq-ffi, which carries this for free. Do it only if the hand-written crate outlives that line.

- `cmake/cargo-build.cmake` already parses cargo's JSON messages to find
  `moq.h` in its hashed `OUT_DIR`. Read the libmoq `compiler-artifact`
  message's staticlib filename from the same output and copy it beside the
  header in the CMake binary dir, `ONLY_IF_DIFFERENT`. `RUST_LIB` then points
  there. Chosen over forcing `--target-dir` into the build dir, which loses
  the shared workspace cache, and over `cargo metadata`, which still guesses
  the triple and profile subdirectories.
- The `BUILD_RUST_LIB=OFF` prebuilt path is unchanged.
- Regression test in CI: the existing `cpp/obs` build against `MOQ_LOCAL`
  runs with `CARGO_TARGET_DIR` outside `target/`, so a hardcoded path fails
  the build. Update `rs/libmoq/README.md` and `doc/lib/c` if they describe
  the path.

## Required

- The maintainer keeps the hand-written libmoq instead of retiring it in the Generated C bindings line

## Related

- [libmoq shutdown](/quest/m3/libmoq-shutdown.md) - the other libmoq packaging fix OBS needs
