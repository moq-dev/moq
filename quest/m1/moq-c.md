# [M] libmoq becomes moq-c

## Goal

The C bindings ship as `moq-c`, matching `moq-cpp` and the other bindings: the
crate, its directory, release tags, CMake package, and pkg-config file all use
the name. C code and link lines do not change: the header stays `moq.h` and the
library file keeps its current name. The published `libmoq` crate gets one last
release that points users at `moq-c`.

## Plan

- Rename the crate `libmoq` to `moq-c` (`rs/libmoq` to `rs/moq-c`), its release
  tags from `libmoq-v*` to `moq-c-v*`, and its workflow. The CMake package
  becomes `find_package(moq-c)` and the pkg-config file `moq-c.pc`. The C++
  package is already `moq-cpp` (#4187), so the two install side by side.
- Keep the `[lib] name` so the library file and `moq.h` are unchanged.
- A published package rename is a break, so this lands on `dev`. Release
  tooling (release-plz, alert and nightly workflows, cachix) must follow the
  new name and tag; check every workflow that names libmoq.
- Update every consumer and reference: `cpp/obs` (`find_package`), interop
  clients, `doc/lib/c`, `doc/bin/obs.md`, the root `CLAUDE.md` Cross-Package
  Sync table, and quests that name libmoq. Grep the whole repository.
- Publish a final `libmoq` release whose README and description point at
  `moq-c`, then stop publishing it. The maintainer cuts releases; the PR only
  prepares it.

Public API: the C ABI is unchanged; the crate, package, and tag names change.
Wire: none.

## Related

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the `moq-cpp` package this name mirrors
