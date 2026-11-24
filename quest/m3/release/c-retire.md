# [S] Retire the hand-written libmoq crate

## Goal

The hand-written C crate, `libmoq`, is gone: its source, workflows, and docs
that only served it are deleted once the generated `moq-c` publishes. The
generated `moq-c` package's `MIGRATE.md` tells C users how to move, and the
last `libmoq` on crates.io and the last `libmoq-v*` release say `moq-c`
replaces them.

## Plan

- Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): the
  hand-written API ships at the cut as `libmoq` 0.22, its final line, and
  `moq-c` names only the generated package
  (`c/moq`). This replaces the
  2026-10-25 decision that it ships as `moq-c`, and with it the code-free
  `rs/libmoq` stub and its two quests: there is no stub at this cut.
- So the crate this deletes is `rs/libmoq`. Before its source goes, publish
  the pointer: a final `libmoq` patch whose README and description name
  `moq-c`, as [retired names](/quest/m3/release/retired-names.md) does for
  the other renames, since the hand-written crate is a published package
  when the generated one replaces it. Its `MIGRATE.md` history carries into
  the generated package.
- The crate's `native-libs/` serves the generated package too, and
  `cpp/moq/cmake/moq-ffi.cmake` serves both C++ and C. Move them to a neutral
  home shared by both packages before deleting the crate, update every
  reader of either path (`rg 'native-libs|moq-ffi\.cmake'`), and check
  that the installed C and C++ CMake configs still link the native libraries.
- Deleting the crate removes its four per-target archives from the
  Nightly size report, which `sh/size/report.py` refuses as missing. Add a way
  to retire one artifact's series without dropping the rest of the history
  (decided 2026-10-28, from [moq-dev/moq#7132](https://github.com/moq-dev/moq/pull/7132)),
  rather than deleting every `nightly-size-history` artifact.

## Required

- [Generated C publication](/quest/m3/release/c-publish.md) - the replacement is available and verified, and the smoke client checks it

- [C consumers](/quest/m3/c/consumers.md) - the interop client and `doc/lib/c` samples no longer call the hand-written ABI

