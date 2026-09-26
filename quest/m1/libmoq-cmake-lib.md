# [XS] libmoq CMake finds the library cargo built

## Goal

`rs/libmoq/CMakeLists.txt` links the static library cargo actually produced,
whatever `CARGO_TARGET_DIR`, `--target` triple, or profile the build uses.
Today `BUILD_RUST_LIB` assumes `target/<debug|release>/libmoq.a`, so any other
layout links a stale library or fails to find one.

## Plan

- `cmake/cargo-build.cmake` already parses cargo's JSON messages to find
  `moq.h` in its hashed `OUT_DIR`. Read the libmoq `compiler-artifact`
  message's staticlib filename from the same output and copy it beside the
  header in the CMake binary dir, `ONLY_IF_DIFFERENT`. `RUST_LIB` then points
  there. Chosen over forcing `--target-dir` into the build dir, which loses
  the shared workspace cache, and over `cargo metadata`, which still guesses
  the triple and profile subdirectories.
- The `BUILD_RUST_LIB=OFF` prebuilt path is unchanged.
- Verify by building `cpp/obs` against `MOQ_LOCAL` with `CARGO_TARGET_DIR`
  set elsewhere. Update `rs/libmoq/README.md` and `doc/lib/c` if they describe
  the path.

## Related

- [libmoq shutdown](/quest/m1/libmoq-shutdown.md) - the other libmoq packaging fix OBS needs
