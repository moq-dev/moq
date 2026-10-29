# [S] Retire the hand-written moq-c crate

## Goal

The hand-written C crate is gone: its source, workflows, and docs that only
served it are deleted once the generated `moq-c` publishes. The generated
`moq-c` package's `MIGRATE.md` tells C users how to move.

## Plan

- #4288 renamed libmoq to `moq-c` (`rs/moq-c`); this quest deletes that
  hand-written crate.
- Decided 2026-10-25 (maintainer): the hand-written `moq-c` 0.7 ships at the
  m1 cut, so it is a published package when the generated 0.8 replaces it.
  Its `MIGRATE.md` history carries into the generated package. This replaces
  the 2026-10-20 premise that it never published.
- `rs/moq-c/native-libs/` serves the generated package too, and
  `cpp/moq/cmake/moq-ffi.cmake` serves both C++ and C. Move them to a neutral
  home shared by both packages before deleting the crate, update every
  reader of either path (`rg 'moq-c/native-libs|moq-ffi\.cmake'`), and check
  that the installed C and C++ CMake configs still link the native libraries.
- `AGENTS.md`'s Cross-Package Sync table keeps the hand-written crate beside
  `c/moq` while it ships. Drop `rs/moq-c` from the `rs/moq-ffi` row and point
  the `rs/moq-c` C ABI row at the generated `c/moq` header (`<moq/moq.h>`).
- Deleting the crate removes its four `moq-c-<target>` archives from the
  Nightly size report, which `sh/size/report.py` refuses as missing. Add a way
  to retire one artifact's series without dropping the rest of the history
  (decided 2026-10-28, from [moq-dev/moq#7132](https://github.com/moq-dev/moq/pull/7132)),
  rather than deleting every `nightly-size-history` artifact.
- The code-free `rs/libmoq` stub left under the old name is
  [Retire the libmoq stub](/quest/m2/release/libmoq-retire.md)'s, separately.

## Required

- [Generated C publication](/quest/m2/release/c-publish.md) - the replacement is available and verified

- [C consumers](/quest/m2/c/consumers.md) - nothing in the repository still calls the hand-written ABI

## Related

- [Retire the libmoq stub](/quest/m2/release/libmoq-retire.md) - deletes the `rs/libmoq` stub under the old name
