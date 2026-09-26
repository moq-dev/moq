# [M] moq-c package from the generated C

## Goal

The generated C ships as the `moq-c` package, version 0.8.0: a release archive
with the header, the moq-ffi staticlib, a CMake config exporting `moq::c`, and
`moq-c.pc`, built the same way `cpp/moq` builds the C++ package. It replaces the
hand-written crate's artifacts under the same names.

## Plan

- Mirror `cpp/moq`: CMake runs `cargo build -p moq-ffi`, then the generator's C
  backend, and installs the result; a probe test compiles and links from the
  installed package through both `find_package(moq-c)` and pkg-config.
- A release workflow and nightly/CI jobs follow the C++ package's, pinned to the
  fork tag that carries the C backend.
- Every Cross-Package Sync row that names libmoq for moq-ffi changes points at
  the generated package instead; update `AGENTS.md` in this quest.

## Required

- [C backend](/quest/m1/c/backend.md) - the generator output this packages
