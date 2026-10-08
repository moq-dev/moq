# [M] moq-c package from the generated C

## Goal

The generated C ships as the `moq-c` package, version 0.8.0: a release archive
with the header, the moq-ffi staticlib, a CMake config exporting `moq::c`, and
`moq-c.pc`, built the same way `cpp/moq` builds the C++ package. It replaces the
hand-written `rs/moq-c` crate's artifacts (renamed from libmoq by #4288)
under the same names.

## Plan

- Mirror `cpp/moq`: CMake runs `cargo build -p moq-ffi`, then the generator's C
  backend, and installs the result; a probe test compiles and links from the
  installed package through both `find_package(moq-c)` and pkg-config.
- The installed CMake config honors `CMAKE_INSTALL_LIBDIR`, so a `lib64`
  distro gets a working `find_package`; the hand-written crate's config
  hardcodes `<prefix>/lib`.
- `doc/index.md` says the C library ships static and shared; say what the
  package actually ships.
- A release workflow and nightly/CI jobs follow the C++ package's, pinned to the
  fork tag that carries the C backend, `v0.11.0-kixelated.4+v0.32.2`
  (`--lang c`).
- Ship a hand-written `moq_shutdown` that calls `moq_ffi_shutdown`, then
  `moq_shutdown_dispatcher`.
- moq-c and moq-cpp each embed the moq-ffi staticlib, so one program can't link
  both; say so in the docs.
- Every Cross-Package Sync row that names libmoq for moq-ffi changes points at
  the generated package instead; update `AGENTS.md` in this quest.

## Required

- [FFI shape](/quest/m1/ffi-shape/README.md) - moq-c 0.8.0 ships the reshaped moq-ffi, so C breaks once
- [C++ package](/quest/m1/cpp/README.md) - the `cpp/moq` build, generator pin, and release workflow this mirrors (#4079)
